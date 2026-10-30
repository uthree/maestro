//! The workflow engine: commanders' plans in, workers started in dependency order, work kept.
//!
//! One thread owns the engine and takes [`Job`]s from a channel: commands from frontends, and
//! every agent event the runtime sees. Doing all of it on one thread keeps the bookkeeping (which
//! agent is on which node, when it started, how often the user asked for changes) free of locks.
//! The hub's lock is taken only to read or edit a workflow, never while `git` or a CLI runs.
//!
//! A node's life:
//!
//! 1. It becomes ready when every node it depends on has succeeded, and every node of an earlier
//!    workflow it waits on too. A commander's plan is set going as soon as it is adopted, so work
//!    asked for while other work runs starts by itself once what it builds on is done.
//! 2. The router picks its model, unless the user or commander fixed one; a worktree is made on
//!    its own branch from the workflow's base, and its dependencies' branches are merged in, with
//!    those of the earlier workflows' nodes it waits on in the same repository.
//! 3. A worker starts there with the node's instructions.
//! 4. When the worker's turn completes, its changes are committed on the node's branch, the node
//!    succeeds, its outcome is recorded for the router, and the nodes waiting on it start, in
//!    whichever workflow they are.
//! 5. A message from the user to the node reopens it and counts as a revision request; the next
//!    completion commits again and updates the outcome.
//!
//! When there is room (see [`maestro_core::Comparison`]), step 2 picks several models instead of
//! one, and each gets its own worktree on its own branch (`maestro/wf1-n2-c1`, `-c2`) and its own
//! worker. Every contender's outcome is recorded. Once they have all finished, the node waits
//! for the user to pick whose work to keep ([`Command::ChooseContender`]); the pick is recorded
//! as a win for that model and a loss for the others, the others stop, and the node succeeds
//! with the chosen branch, so its dependents merge that work.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use maestro_core::{
    AgentEvent, AgentId, AgentRole, AgentSpec, COMMANDER_INSTRUCTIONS, Contender, Difficulty, Host,
    ModelChoice, ModelKey, Node, NodeId, NodeStatus, Outcome, Plan, ProviderKind,
    WORKER_INSTRUCTIONS, Workflow, WorkflowId, WorkflowTarget, Workspace, Worktree,
};
use maestro_host::github::{self, PullRequestDraft};
use maestro_host::{HostCommand, git};
use maestro_protocol::{Command, CommandError, Event, Reply};

use crate::runtime::{Runtime, failed, prepare_worktree};
use crate::store::Store;
use crate::{Inner, broadcast};

pub(crate) enum Job {
    Command(Box<Command>, Sender<Result<Reply, CommandError>>),
    Agent(AgentId, AgentEvent),
}

/// Whether `command` belongs to the engine.
pub(crate) fn handles(command: &Command) -> bool {
    matches!(
        command,
        Command::RunWorkflow { .. }
            | Command::SendToNode { .. }
            | Command::RateNode { .. }
            | Command::ChooseContender { .. }
            | Command::StartCommander { .. }
            | Command::ChooseModel { .. }
            | Command::PublishWorkflow { .. }
    )
}

struct Worker {
    workflow: WorkflowId,
    node: NodeId,
    host: Host,
    path: PathBuf,
    model: ModelKey,
    difficulty: Difficulty,
    tags: Vec<String>,
    started: Instant,
    revisions: u32,
    /// Where its outcome is recorded.
    key: String,
    /// One of several workers on the node, until the user picks it.
    contender: bool,
    /// In a turn, so using the host's compute and the provider's quota.
    busy: bool,
}

/// What every worker on a node starts from.
struct Brief {
    /// The work merged into its worktree first: a branch, the title an error names it by, and
    /// how the worker is told about it.
    merges: Vec<(String, String, String)>,
    /// Its first message.
    prompt: String,
}

struct Engine {
    inner: Arc<Mutex<Inner>>,
    runtime: Arc<Runtime>,
    store: Store,
    workers: HashMap<AgentId, Worker>,
    /// Each commander, with the target the workflows it plans get.
    commanders: HashMap<AgentId, Option<WorkflowTarget>>,
    /// Which CLIs each host has, looked up once.
    providers: HashMap<Host, Vec<ProviderKind>>,
    published: Vec<WorkflowId>,
}

/// Starts the engine's thread and returns its inbox.
pub(crate) fn spawn(inner: Arc<Mutex<Inner>>, runtime: Arc<Runtime>, store: Store) -> Sender<Job> {
    let (jobs, inbox) = mpsc::channel();
    let events = Mutex::new(jobs.clone());
    runtime.listen(Box::new(move |agent, event| {
        events.lock().expect("engine inbox").send(Job::Agent(agent, event)).ok();
    }));
    let mut engine = Engine {
        inner,
        runtime,
        store,
        workers: HashMap::new(),
        commanders: HashMap::new(),
        providers: HashMap::new(),
        published: Vec::new(),
    };
    thread::spawn(move || {
        for job in inbox {
            match job {
                Job::Command(command, reply) => {
                    reply.send(engine.execute(*command)).ok();
                }
                Job::Agent(agent, event) => engine.observe(agent, event),
            }
        }
    });
    jobs
}

impl Engine {
    fn execute(&mut self, command: Command) -> Result<Reply, CommandError> {
        match command {
            Command::RunWorkflow { workflow } => {
                if self.snapshot(workflow)?.target.is_none() {
                    return Err(refused("set the workflow's target before running it"));
                }
                self.update(workflow, |flow| flow.started = true)?;
                self.advance(workflow);
                Ok(Reply::Done)
            }
            Command::SendToNode { workflow, node, text } => {
                let agent = self
                    .snapshot(workflow)?
                    .node(node)
                    .ok_or_else(|| refused(format!("node {node} is not in this workflow")))?
                    .agent
                    .ok_or_else(|| refused("the node has not started; edit it instead"))?;
                let Some(worker) = self.workers.get_mut(&agent) else {
                    // Its worker has exited, perhaps with an earlier daemon: start another.
                    return self.revive(workflow, node, text);
                };
                worker.revisions += 1;
                worker.busy = true;
                self.set_status(workflow, node, NodeStatus::Running);
                self.runtime.execute(Command::SendToAgent { agent, text })
            }
            Command::RateNode { workflow, node, rating } => {
                if !(1..=5).contains(&rating) {
                    return Err(refused("a rating is from 1 to 5"));
                }
                let flow = self.snapshot(workflow)?;
                let node = flow
                    .node(node)
                    .ok_or_else(|| refused(format!("node {node} is not in this workflow")))?;
                if !self.store.rate(&node_key(workflow, node), rating) {
                    return Err(refused("the node has not finished yet"));
                }
                Ok(Reply::Done)
            }
            Command::StartCommander { model, mut workspace, target, prompt } => {
                workspace.worktree = None;
                let spec = AgentSpec { role: AgentRole::Commander, model, workspace };
                let agent = self.runtime.start(spec, COMMANDER_INSTRUCTIONS.into(), &prompt)?;
                self.commanders.insert(agent, target);
                Ok(Reply::AgentStarted { agent })
            }
            Command::ChooseContender { workflow, node, agent } => {
                self.choose(workflow, node, agent)
            }
            Command::ChooseModel { difficulty, tags, host } => {
                let available = self.providers(&host);
                let outcomes = self.store.outcomes();
                let choice = self.store.router.choose(difficulty, &tags, &outcomes, &available);
                Ok(Reply::ModelChosen { choice })
            }
            Command::PublishWorkflow { workflow } => self.publish(workflow),
            other => unreachable!("not an engine command: {other:?}"),
        }
    }

    fn observe(&mut self, agent: AgentId, event: AgentEvent) {
        if self.commanders.contains_key(&agent) {
            match event {
                AgentEvent::TurnCompleted { answer } => self.adopt_plans(agent, &answer),
                AgentEvent::Exited { .. } => {
                    self.commanders.remove(&agent);
                }
                _ => {}
            }
            return;
        }
        let Some(worker) = self.workers.get(&agent) else { return };
        let (workflow, node, contender) = (worker.workflow, worker.node, worker.contender);
        match event {
            AgentEvent::PermissionRequested { .. } if contender => {
                self.settle(agent, NodeStatus::AwaitingUser);
            }
            // A message the user sent during the turn that just finished opens the node again.
            AgentEvent::Prompted { .. } if !contender => {
                let status =
                    self.snapshot(workflow).ok().and_then(|flow| flow.node(node).map(|n| n.status));
                if matches!(status, Some(NodeStatus::Succeeded | NodeStatus::Failed)) {
                    self.set_status(workflow, node, NodeStatus::Running);
                }
            }
            AgentEvent::PermissionRequested { .. } => {
                self.set_status(workflow, node, NodeStatus::AwaitingUser);
            }
            AgentEvent::Text { .. } | AgentEvent::ToolStarted { .. } => {
                if self.status_of(agent) == Some(NodeStatus::AwaitingUser) {
                    if contender {
                        self.settle(agent, NodeStatus::Running);
                    } else {
                        self.set_status(workflow, node, NodeStatus::Running);
                    }
                }
            }
            AgentEvent::TurnCompleted { .. } => self.finish(agent),
            AgentEvent::TurnFailed { message } => {
                self.record(agent, false);
                if contender {
                    self.settle(agent, NodeStatus::Failed);
                } else {
                    self.fail(workflow, node, message);
                }
            }
            AgentEvent::Exited { reason } => {
                let status = self.status_of(agent);
                let working =
                    matches!(status, Some(NodeStatus::Running | NodeStatus::AwaitingUser));
                if working && contender {
                    self.record(agent, false);
                    self.settle(agent, NodeStatus::Failed);
                }
                self.workers.remove(&agent);
                if working && !contender {
                    let message = reason.unwrap_or_else(|| "the worker exited".into());
                    self.fail(workflow, node, message);
                }
            }
            _ => {}
        }
    }

    /// Where a worker's work stands: its contender's status while it is one, else its node's.
    fn status_of(&self, agent: AgentId) -> Option<NodeStatus> {
        let worker = self.workers.get(&agent)?;
        let flow = self.snapshot(worker.workflow).ok()?;
        let node = flow.node(worker.node)?;
        if worker.contender {
            node.contenders.iter().find(|c| c.agent == agent).map(|c| c.status)
        } else {
            Some(node.status)
        }
    }

    /// Sets a contender's status, and acts on what that makes of its node.
    fn settle(&mut self, agent: AgentId, status: NodeStatus) {
        let Some(worker) = self.workers.get(&agent) else { return };
        let (workflow, node) = (worker.workflow, worker.node);
        let settled = self.update(workflow, |flow| flow.set_contender_status(node, agent, status));
        match settled {
            // Only one contender's work succeeded, so it was kept without asking.
            Ok(Ok(NodeStatus::Succeeded)) => {
                if let Some(worker) = self.workers.get_mut(&agent) {
                    worker.contender = false;
                }
                self.succeeded(workflow);
            }
            Ok(Ok(NodeStatus::Failed)) => {
                self.fail(workflow, node, "every model's attempt at it failed".into());
            }
            _ => {}
        }
    }

    /// Keeps the work of the contender the user picked, records the pick, and stops the others.
    fn choose(
        &mut self,
        workflow: WorkflowId,
        node: NodeId,
        agent: AgentId,
    ) -> Result<Reply, CommandError> {
        let contenders = self.update(workflow, |flow| {
            flow.choose_contender(node, agent)?;
            Ok(flow.node(node).map(|node| node.contenders.clone()).unwrap_or_default())
        })?;
        let contenders: Vec<Contender> =
            contenders.map_err(|error: maestro_core::WorkflowError| refused(error.to_string()))?;
        for contender in &contenders {
            let key = contender_key(workflow, node, contender.agent);
            if contender.agent == agent {
                self.store.judge(&key, true);
            } else if contender.status == NodeStatus::Succeeded {
                self.store.judge(&key, false);
            }
            if contender.agent != agent && self.workers.contains_key(&contender.agent) {
                self.runtime.execute(Command::StopAgent { agent: contender.agent }).ok();
            }
        }
        if let Some(worker) = self.workers.get_mut(&agent) {
            worker.contender = false;
        }
        self.succeeded(workflow);
        Ok(Reply::Done)
    }

    /// Commits a worker's turn, marks its node succeeded, and starts what was waiting on it.
    fn finish(&mut self, agent: AgentId) {
        let Some(worker) = self.workers.get_mut(&agent) else { return };
        worker.busy = false;
        let (workflow, node, host, path, contender) = (
            worker.workflow,
            worker.node,
            worker.host.clone(),
            worker.path.clone(),
            worker.contender,
        );
        let title = self
            .snapshot(workflow)
            .ok()
            .and_then(|flow| flow.node(node).map(|node| node.title.clone()))
            .unwrap_or_default();
        if let Err(error) = git::commit_all(&host, &path, &title) {
            self.record(agent, false);
            if contender {
                return self.settle(agent, NodeStatus::Failed);
            }
            return self.fail(workflow, node, format!("could not commit the work: {error}"));
        }
        self.record(agent, true);
        if contender {
            return self.settle(agent, NodeStatus::Succeeded);
        }
        self.set_status(workflow, node, NodeStatus::Succeeded);
        self.succeeded(workflow);
    }

    /// Starts what a node's success let start, and publishes a workflow that is now done.
    fn succeeded(&mut self, workflow: WorkflowId) {
        self.advance_all();

        let Ok(flow) = self.snapshot(workflow) else { return };
        let publish = flow.target.as_ref().is_some_and(|target| target.publish);
        let done = flow.nodes().all(|node| node.status == NodeStatus::Succeeded);
        if publish
            && done
            && !self.published.contains(&workflow)
            && let Err(error) = self.publish(workflow)
        {
            eprintln!("maestro-daemon: could not publish {workflow}: {error}");
        }
    }

    /// Starts every ready node of every workflow that has been set going.
    fn advance_all(&mut self) {
        let started: Vec<WorkflowId> = {
            let inner = self.inner.lock().expect("hub lock");
            inner.state.workflows().iter().filter(|flow| flow.started).map(|flow| flow.id).collect()
        };
        for workflow in started {
            self.advance(workflow);
        }
    }

    /// Starts every ready node of a workflow.
    fn advance(&mut self, workflow: WorkflowId) {
        let Ok(flow) = self.snapshot(workflow) else { return };
        let ready = {
            let inner = self.inner.lock().expect("hub lock");
            flow.ready(|other| inner.state.status(other) == Some(NodeStatus::Succeeded))
        };
        for node in ready {
            if let Err(message) = self.launch(&flow, node) {
                self.fail(workflow, node, message);
            }
        }
    }

    fn launch(&mut self, flow: &Workflow, id: NodeId) -> Result<(), String> {
        let target = flow.target.clone().ok_or("the workflow has no target")?;
        let node = flow.node(id).ok_or("the node is gone")?;
        let difficulty = node.difficulty.unwrap_or(Difficulty::Moderate);
        let models: Vec<ModelChoice> = match &node.model {
            Some(model) => vec![model.clone()],
            None => {
                let available = self.providers(&target.host);
                let busy = self.busy(&target.host);
                let outcomes = self.store.outcomes();
                let router = &self.store.router;
                router
                    .lineup(difficulty, &node.tags, &outcomes, &available, &busy)
                    .into_iter()
                    .map(|choice| choice.model)
                    .collect()
            }
        };
        if models.is_empty() {
            return Err("no model in the router's catalog is installed on the host".into());
        }

        // The work to merge into each worktree first: what it depends on in this workflow, and
        // earlier work it waits on when that is in the same repository. Each is a branch, the
        // title to name it by in an error, and how the worker is told about it.
        let mut merges: Vec<(String, String, String)> = Vec::new();
        for dependency in flow.dependencies(id) {
            let dependency = flow.node(dependency).ok_or("a dependency is gone")?;
            let Some(theirs) = &dependency.branch else { continue };
            merges.push((theirs.clone(), dependency.title.clone(), dependency.title.clone()));
        }
        for other in &node.after {
            let Ok(earlier) = self.snapshot(other.workflow) else { continue };
            let same = earlier.target.as_ref().is_some_and(|theirs| {
                theirs.host == target.host && theirs.repository == target.repository
            });
            let Some(dependency) = earlier.node(other.node).filter(|_| same) else { continue };
            let Some(theirs) = &dependency.branch else { continue };
            let told = format!("{} (from \"{}\")", dependency.title, earlier.title);
            merges.push((theirs.clone(), dependency.title.clone(), told));
        }
        let done: Vec<&str> = merges.iter().map(|(_, _, told)| told.as_str()).collect();
        let mut prompt = format!("This step is part of \"{}\".", flow.title);
        if !done.is_empty() {
            prompt.push_str(&format!(" Already done and merged here: {}.", done.join(", ")));
        }
        prompt.push_str(&format!("\n\n# {}\n\n{}", node.title, node.instructions));

        let brief = Brief { merges, prompt };
        let branch = format!("maestro/{}-{}", flow.id, id);
        if let [model] = &models[..] {
            let agent = self.start_worker(flow, node, model, &branch, &brief, false)?;
            let model = model.clone();
            self.update(flow.id, |flow| flow.assign(id, agent, model, branch).ok()).ok();
            return Ok(());
        }

        let mut contenders = Vec::new();
        let mut errors = Vec::new();
        for (index, model) in models.into_iter().enumerate() {
            let branch = format!("{branch}-c{}", index + 1);
            match self.start_worker(flow, node, &model, &branch, &brief, true) {
                Ok(agent) => {
                    contenders.push(Contender { agent, model, branch, status: NodeStatus::Running })
                }
                Err(error) => errors.push(error),
            }
        }
        match contenders.len() {
            0 => Err(errors.join("; ")),
            1 => {
                // Nothing to compare with: it is the node's only worker.
                let Contender { agent, model, branch, .. } = contenders.remove(0);
                if let Some(worker) = self.workers.get_mut(&agent) {
                    worker.contender = false;
                    worker.key = outcome_key(flow.id, id);
                }
                self.update(flow.id, |flow| flow.assign(id, agent, model, branch).ok()).ok();
                Ok(())
            }
            _ => {
                self.update(flow.id, |flow| flow.assign_contenders(id, contenders).ok()).ok();
                Ok(())
            }
        }
    }

    /// Makes a worktree on `branch` with the node's dependencies merged in, and starts a worker
    /// there.
    fn start_worker(
        &mut self,
        flow: &Workflow,
        node: &Node,
        model: &ModelChoice,
        branch: &str,
        brief: &Brief,
        contender: bool,
    ) -> Result<AgentId, String> {
        let target = flow.target.as_ref().ok_or("the workflow has no target")?;
        let workspace = node_workspace(target, branch);
        prepare_worktree(&workspace).map_err(|error| error.to_string())?;
        let path = workspace.working_directory().clone();
        for (theirs, title, _) in &brief.merges {
            git::merge(&target.host, &path, theirs)
                .map_err(|error| format!("merging the work of {title:?} conflicted: {error}"))?;
        }

        let spec = AgentSpec { role: AgentRole::Worker, model: model.clone(), workspace };
        let agent = self
            .runtime
            .start(spec, WORKER_INSTRUCTIONS.into(), &brief.prompt)
            .map_err(|error| error.to_string())?;
        let key = if contender {
            contender_key(flow.id, node.id, agent)
        } else {
            outcome_key(flow.id, node.id)
        };
        self.workers.insert(
            agent,
            Worker {
                workflow: flow.id,
                node: node.id,
                host: target.host.clone(),
                path,
                model: ModelKey { provider: model.provider, model: model.model.clone() },
                difficulty: node.difficulty.unwrap_or(Difficulty::Moderate),
                tags: node.tags.clone(),
                started: Instant::now(),
                revisions: 0,
                key,
                contender,
                busy: true,
            },
        );
        Ok(agent)
    }

    /// The provider of every worker in a turn on `host`, once per worker.
    fn busy(&self, host: &Host) -> Vec<ProviderKind> {
        self.workers
            .values()
            .filter(|worker| worker.busy && worker.host == *host)
            .map(|worker| worker.model.provider)
            .collect()
    }

    /// Starts a new worker on a node whose worker has exited, in the node's worktree, with what
    /// the user now asks.
    fn revive(
        &mut self,
        workflow: WorkflowId,
        id: NodeId,
        text: String,
    ) -> Result<Reply, CommandError> {
        let flow = self.snapshot(workflow)?;
        let target = flow.target.clone().ok_or_else(|| refused("the workflow has no target"))?;
        let node =
            flow.node(id).ok_or_else(|| refused(format!("node {id} is not in this workflow")))?;
        let (Some(branch), Some(model)) = (node.branch.clone(), node.model.clone()) else {
            return Err(refused("the node has not started; edit it instead"));
        };
        let workspace = node_workspace(&target, &branch);
        prepare_worktree(&workspace).map_err(failed)?;
        let path = workspace.working_directory().clone();
        let prompt = format!(
            "This step is part of \"{}\". You worked on it before, and that work is committed on \
             this branch.\n\n# {}\n\n{}\n\n# What the user asks now\n\n{text}",
            flow.title, node.title, node.instructions
        );
        let spec = AgentSpec { role: AgentRole::Worker, model: model.clone(), workspace };
        let agent = self.runtime.start(spec, WORKER_INSTRUCTIONS.into(), &prompt)?;
        self.workers.insert(
            agent,
            Worker {
                workflow,
                node: id,
                host: target.host.clone(),
                path,
                model: ModelKey { provider: model.provider, model: model.model.clone() },
                difficulty: node.difficulty.unwrap_or(Difficulty::Moderate),
                tags: node.tags.clone(),
                started: Instant::now(),
                revisions: 1,
                key: node_key(workflow, node),
                contender: false,
                busy: true,
            },
        );
        self.update(workflow, |flow| flow.assign(id, agent, model, branch).ok())?;
        Ok(Reply::Done)
    }

    /// Merges every node's branch into one, pushes it and opens a pull request.
    fn publish(&mut self, workflow: WorkflowId) -> Result<Reply, CommandError> {
        let flow = self.snapshot(workflow)?;
        let target = flow.target.clone().ok_or_else(|| refused("the workflow has no target"))?;
        if let Some(node) = flow.nodes().find(|node| node.status != NodeStatus::Succeeded) {
            return Err(refused(format!("{:?} has not succeeded yet", node.title)));
        }
        let branch = format!("maestro/{}", flow.id);
        let workspace = Workspace {
            host: target.host.clone(),
            repository: target.repository.clone(),
            worktree: Some(Worktree {
                branch: branch.clone(),
                base: target.base.clone(),
                path: target.worktrees.join(branch.replace('/', "-")),
            }),
        };
        prepare_worktree(&workspace).map_err(failed)?;
        let path = workspace.working_directory();
        let mut body = String::from("Planned and carried out by Maestro.\n");
        for id in flow.topological_order() {
            let node = flow.node(id).expect("ordered nodes exist");
            if let Some(theirs) = &node.branch {
                git::merge(&target.host, path, theirs).map_err(failed)?;
            }
            let model = node.model.as_ref().map_or_else(String::new, |model| {
                format!(" ({} {})", model.provider.label(), model.model).replace(" )", ")")
            });
            body.push_str(&format!("\n- **{}**{model}", node.title));
        }
        git::push(&target.host, path, &branch).map_err(failed)?;
        let draft = PullRequestDraft {
            title: flow.title.clone(),
            body,
            base: target.base.clone(),
            head: branch,
            draft: false,
        };
        let opened = github::create(&target.host, path, &draft).map_err(failed)?;
        self.published.push(workflow);
        let event =
            Event::WorkflowPublished { workflow, number: opened.number, url: opened.url.clone() };
        broadcast(&mut self.inner.lock().expect("hub lock"), &event);
        Ok(Reply::PullRequestOpened { number: opened.number, url: opened.url, commit: None })
    }

    /// Turns every plan in a commander's reply into a workflow, and tells the commander about
    /// any it got wrong so it can write them again.
    fn adopt_plans(&mut self, agent: AgentId, answer: &str) {
        let target = self.commanders.get(&agent).cloned().flatten();
        let mut adopted = Vec::new();
        for plan in Plan::extract(answer) {
            let mut inner = self.inner.lock().expect("hub lock");
            let built = plan.and_then(|plan| {
                let state = &inner.state;
                let mut workflow =
                    plan.into_workflow(state.next_id(), target.clone(), |other| {
                        state.contains(other)
                    })?;
                // The user agreed to the plan before the commander wrote it: set it going.
                workflow.started = workflow.target.is_some();
                workflow.planned_by = Some(agent);
                Ok(inner.state.insert(workflow).clone())
            });
            match built {
                Ok(workflow) => {
                    let id = workflow.id;
                    broadcast(&mut inner, &Event::WorkflowChanged { workflow });
                    broadcast(&mut inner, &Event::Planned { agent, workflow: id });
                    adopted.push(id);
                }
                Err(error) => {
                    let message = error.to_string();
                    broadcast(&mut inner, &Event::PlanRejected { agent, message: message.clone() });
                    drop(inner);
                    let text = format!(
                        "Maestro could not use that plan: {message}. Please write the whole plan again."
                    );
                    self.runtime.execute(Command::SendToAgent { agent, text }).ok();
                }
            }
        }
        for workflow in adopted {
            self.advance(workflow);
        }
    }

    fn record(&mut self, agent: AgentId, succeeded: bool) {
        let Some(worker) = self.workers.get(&agent) else { return };
        let outcome = Outcome {
            model: worker.model.clone(),
            difficulty: worker.difficulty,
            succeeded,
            seconds: worker.started.elapsed().as_secs_f64(),
            revisions: worker.revisions,
            rating: None,
            tags: worker.tags.clone(),
            chosen: None,
        };
        self.store.record(worker.key.clone(), outcome);
    }

    fn fail(&mut self, workflow: WorkflowId, node: NodeId, message: String) {
        self.set_status(workflow, node, NodeStatus::Failed);
        let event = Event::NodeFailed { workflow, node, message };
        broadcast(&mut self.inner.lock().expect("hub lock"), &event);
    }

    fn set_status(&self, workflow: WorkflowId, node: NodeId, status: NodeStatus) {
        self.update(workflow, |flow| flow.set_status(node, status).ok()).ok();
    }

    /// Edits a workflow under the hub's lock and sends every frontend the result.
    fn update<T>(
        &self,
        workflow: WorkflowId,
        change: impl FnOnce(&mut Workflow) -> T,
    ) -> Result<T, CommandError> {
        let mut inner = self.inner.lock().expect("hub lock");
        let flow = inner.state.workflow(workflow)?;
        let result = change(flow);
        let event = Event::WorkflowChanged { workflow: flow.clone() };
        broadcast(&mut inner, &event);
        Ok(result)
    }

    fn snapshot(&self, workflow: WorkflowId) -> Result<Workflow, CommandError> {
        Ok(self.inner.lock().expect("hub lock").state.workflow(workflow)?.clone())
    }

    /// The CLIs installed on `host`, found once with `command -v`.
    fn providers(&mut self, host: &Host) -> Vec<ProviderKind> {
        self.providers
            .entry(host.clone())
            .or_insert_with(|| {
                let names = ProviderKind::ALL.map(ProviderKind::executable).join(" ");
                // `command -v` exits non-zero if any is missing but still prints the others.
                let found = HostCommand::new("sh")
                    .args(["-c", &format!("command -v {names}; true")])
                    .run(host)
                    .unwrap_or_default();
                ProviderKind::ALL
                    .into_iter()
                    .filter(|provider| {
                        found
                            .lines()
                            .any(|line| line.rsplit('/').next() == Some(provider.executable()))
                    })
                    .collect()
            })
            .clone()
    }
}

fn outcome_key(workflow: WorkflowId, node: NodeId) -> String {
    format!("{workflow}/{node}")
}

/// Where one contender's outcome is recorded, beside the others on the same node.
fn contender_key(workflow: WorkflowId, node: NodeId, agent: AgentId) -> String {
    format!("{workflow}/{node}/{agent}")
}

/// Where the outcome of the work a node kept is recorded.
fn node_key(workflow: WorkflowId, node: &Node) -> String {
    match node.agent {
        Some(agent) if !node.contenders.is_empty() => contender_key(workflow, node.id, agent),
        _ => outcome_key(workflow, node.id),
    }
}

fn refused(message: impl Into<String>) -> CommandError {
    CommandError::Workflow { message: message.into() }
}

/// The worktree a node's work lives in, on its own branch beside the workflow's repository.
fn node_workspace(target: &WorkflowTarget, branch: &str) -> Workspace {
    Workspace {
        host: target.host.clone(),
        repository: target.repository.clone(),
        worktree: Some(Worktree {
            branch: branch.into(),
            base: target.base.clone(),
            path: target.worktrees.join(branch.replace('/', "-")),
        }),
    }
}
