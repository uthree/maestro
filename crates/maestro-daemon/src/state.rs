use std::collections::BTreeMap;

use std::path::Path;

use maestro_core::{
    AgentId, NodeOrigin, NodeRef, NodeStatus, Project, ProjectError, ProjectId, Resource, Workflow,
    WorkflowError, WorkflowId,
};
use maestro_protocol::{Command, CommandError, Event, Reply, WorkflowSummary};

/// Every project and workflow the daemon knows about.
#[derive(Debug, Default)]
pub struct State {
    projects: BTreeMap<ProjectId, Project>,
    next_project: u64,
    workflows: BTreeMap<WorkflowId, Workflow>,
    next_workflow: u64,
}

/// What a command changed, to send to every frontend.
enum Changed {
    Workflow(WorkflowId),
    Project(ProjectId),
}

impl State {
    /// The state holding `projects` and `workflows`, as a snapshot saved them; new ones get
    /// later ids. Work saved before there were projects goes into one, led by the last of
    /// `commanders`, with the repositories it ran in.
    pub(crate) fn restore(
        projects: Vec<Project>,
        workflows: Vec<Workflow>,
        commanders: &[AgentId],
    ) -> Self {
        let next_project = projects.iter().map(|project| project.id.0).max().unwrap_or(0);
        let next_workflow = workflows.iter().map(|flow| flow.id.0).max().unwrap_or(0);
        let mut state = Self {
            projects: projects.into_iter().map(|project| (project.id, project)).collect(),
            next_project,
            workflows: workflows.into_iter().map(|flow| (flow.id, flow)).collect(),
            next_workflow,
        };
        let loose = state.workflows.values().any(|flow| flow.project.is_none());
        if state.projects.is_empty() && (loose || !commanders.is_empty()) {
            state.adopt_loose_work(commanders.last().copied());
        }
        state
    }

    /// Puts every workflow without a project, and the commander, into a new project.
    fn adopt_loose_work(&mut self, commander: Option<AgentId>) {
        let id = self.new_project_id();
        let targets: Vec<_> =
            self.workflows.values().filter_map(|flow| flow.target.clone()).collect();
        let name = targets
            .first()
            .and_then(|target| Path::new(&target.repository).file_name())
            .map_or_else(|| "Project".into(), |name| name.to_string_lossy().into_owned());
        let mut project = Project::new(id, name);
        project.commander = commander;
        for target in targets {
            let folder = Resource::Directory {
                host: target.host,
                path: target.repository,
                base: target.base,
            };
            project.add(folder).ok();
        }
        for flow in self.workflows.values_mut().filter(|flow| flow.project.is_none()) {
            flow.project = Some(id);
        }
        self.projects.insert(id, project);
    }

    /// Every project, for a snapshot.
    pub(crate) fn projects(&self) -> Vec<Project> {
        self.projects.values().cloned().collect()
    }

    /// Every workflow, for a snapshot.
    pub(crate) fn workflows(&self) -> Vec<Workflow> {
        self.workflows.values().cloned().collect()
    }

    /// Applies one command. On failure nothing has changed and no events are returned.
    pub fn apply(&mut self, command: Command) -> (Result<Reply, CommandError>, Vec<Event>) {
        match self.dispatch(command) {
            Ok((reply, changed)) => {
                let events = changed
                    .into_iter()
                    .map(|changed| match changed {
                        Changed::Workflow(id) => {
                            Event::WorkflowChanged { workflow: self.workflows[&id].clone() }
                        }
                        Changed::Project(id) => {
                            Event::ProjectChanged { project: self.projects[&id].clone() }
                        }
                    })
                    .collect();
                (Ok(reply), events)
            }
            Err(error) => (Err(error), Vec::new()),
        }
    }

    /// Runs a command and names what it changed.
    fn dispatch(&mut self, command: Command) -> Result<(Reply, Vec<Changed>), CommandError> {
        Ok(match command {
            Command::Ping => (Reply::Pong, Vec::new()),
            Command::ListProjects => (Reply::Projects { projects: self.projects() }, Vec::new()),
            Command::CreateProject { name, resources } => {
                let id = self.new_project_id();
                let mut created = Project::new(id, project_name(name)?);
                for resource in resources {
                    created.add(resource).map_err(project_refused)?;
                }
                self.next_project += 1;
                self.projects.insert(id, created);
                (Reply::ProjectCreated { project: id }, vec![Changed::Project(id)])
            }
            Command::RenameProject { project: id, name } => {
                let name = project_name(name)?;
                self.project(id)?.name = name;
                (Reply::Done, vec![Changed::Project(id)])
            }
            Command::AddResource { project: id, resource } => {
                let resource = self.project(id)?.add(resource).map_err(project_refused)?;
                (Reply::ResourceAdded { resource }, vec![Changed::Project(id)])
            }
            Command::RemoveResource { project: id, resource } => {
                let removed = self.project(id)?.remove(resource);
                (Reply::Done, if removed { vec![Changed::Project(id)] } else { Vec::new() })
            }
            Command::ListWorkflows => {
                let workflows = self
                    .workflows
                    .values()
                    .map(|flow| WorkflowSummary {
                        id: flow.id,
                        title: flow.title.clone(),
                        nodes: flow.nodes().count(),
                    })
                    .collect();
                (Reply::Workflows { workflows }, Vec::new())
            }
            Command::GetWorkflow { workflow } => {
                (Reply::Workflow { workflow: self.workflow(workflow)?.clone() }, Vec::new())
            }
            Command::CreateWorkflow { title, project } => {
                if let Some(project) = project {
                    self.project(project)?;
                }
                self.next_workflow += 1;
                let id = WorkflowId(self.next_workflow);
                let mut flow = Workflow::new(id, title);
                flow.project = project;
                self.workflows.insert(id, flow);
                (Reply::WorkflowCreated { workflow: id }, vec![Changed::Workflow(id)])
            }
            Command::AddNode { workflow, node } => {
                let node = self.workflow(workflow)?.add_node(node);
                (Reply::NodeAdded { node }, vec![Changed::Workflow(workflow)])
            }
            Command::RemoveNode { workflow, node } => {
                self.workflow(workflow)?.remove_node(node).map_err(refused)?;
                (Reply::Done, vec![Changed::Workflow(workflow)])
            }
            Command::AddEdge { workflow, from, to } => {
                self.workflow(workflow)?.add_edge(from, to).map_err(refused)?;
                (Reply::Done, vec![Changed::Workflow(workflow)])
            }
            Command::RemoveEdge { workflow, from, to } => {
                let removed = self.workflow(workflow)?.remove_edge(from, to);
                (Reply::Done, if removed { vec![Changed::Workflow(workflow)] } else { Vec::new() })
            }
            Command::WaitFor { workflow, node, on } => {
                if !self.contains(on) {
                    return Err(CommandError::Workflow { message: format!("{on} does not exist") });
                }
                self.workflow(workflow)?.wait_for(node, on).map_err(refused)?;
                (Reply::Done, vec![Changed::Workflow(workflow)])
            }
            Command::StopWaiting { workflow, node, on } => {
                let stopped = self.workflow(workflow)?.stop_waiting(node, on);
                (Reply::Done, if stopped { vec![Changed::Workflow(workflow)] } else { Vec::new() })
            }
            Command::SetWorkflowTarget { workflow, target } => {
                self.workflow(workflow)?.target = target;
                (Reply::Done, vec![Changed::Workflow(workflow)])
            }
            Command::EditNode { workflow, node, edit } => {
                self.workflow(workflow)?
                    .edit_node(node, edit, NodeOrigin::User)
                    .map_err(refused)?;
                (Reply::Done, vec![Changed::Workflow(workflow)])
            }
            Command::DeleteProject { .. }
            | Command::PlaceWorkflow { .. }
            | Command::StartAgent { .. }
            | Command::ListAgents
            | Command::GetTranscript { .. }
            | Command::SendToAgent { .. }
            | Command::AnswerPermission { .. }
            | Command::InterruptAgent { .. }
            | Command::StopAgent { .. }
            | Command::PublishWorkspace { .. }
            | Command::MergePullRequest { .. }
            | Command::RemoveWorktree { .. }
            | Command::RunWorkflow { .. }
            | Command::SendToNode { .. }
            | Command::RateNode { .. }
            | Command::ChooseContender { .. }
            | Command::StartCommander { .. }
            | Command::ChooseModel { .. }
            | Command::GetRouter
            | Command::SetRouter { .. }
            | Command::ListProviders { .. }
            | Command::SetupProvider { .. }
            | Command::PublishWorkflow { .. } => {
                unreachable!("the hub sends these to the runtime, the engine or the providers")
            }
        })
    }

    /// Whether `node` names a node that exists.
    pub(crate) fn contains(&self, node: NodeRef) -> bool {
        self.workflows.get(&node.workflow).is_some_and(|flow| flow.node(node.node).is_some())
    }

    /// The status of the node `node` names, if it exists.
    pub(crate) fn status(&self, node: NodeRef) -> Option<NodeStatus> {
        Some(self.workflows.get(&node.workflow)?.node(node.node)?.status)
    }

    pub(crate) fn project(&mut self, project: ProjectId) -> Result<&mut Project, CommandError> {
        self.projects.get_mut(&project).ok_or(CommandError::UnknownProject { project })
    }

    /// Records `agent` as the project's commander.
    pub(crate) fn lead(&mut self, project: ProjectId, agent: AgentId) -> Option<Project> {
        let project = self.projects.get_mut(&project)?;
        project.commander = Some(agent);
        Some(project.clone())
    }

    /// The commander's briefing: its project's resources and the state of the project's work.
    pub(crate) fn briefing(&self, project: ProjectId) -> Option<String> {
        let project = self.projects.get(&project)?;
        let workflows = self.workflows.values().filter(|flow| flow.project == Some(project.id));
        let mut briefing = project.briefing();
        if let Some(report) = maestro_core::status_report(workflows) {
            briefing.push_str("\n\n");
            briefing.push_str(&report);
        }
        Some(briefing)
    }

    /// Deletes a project and its workflows, unless any of them is busy or waited on from
    /// elsewhere. Returns the workflows removed.
    pub(crate) fn delete_project(
        &mut self,
        project: ProjectId,
    ) -> Result<Vec<WorkflowId>, CommandError> {
        self.project(project)?;
        let doomed: Vec<WorkflowId> = self
            .workflows
            .values()
            .filter(|flow| flow.project == Some(project))
            .map(|flow| flow.id)
            .collect();
        let busy = doomed.iter().any(|id| {
            self.workflows[id].nodes().any(|node| {
                matches!(
                    node.status,
                    NodeStatus::Running | NodeStatus::AwaitingUser | NodeStatus::AwaitingChoice
                )
            })
        });
        if busy {
            return Err(CommandError::Project {
                message: "the project has work under way; wait for it or stop it first".into(),
            });
        }
        let waited =
            self.workflows.values().filter(|flow| !doomed.contains(&flow.id)).any(|flow| {
                flow.nodes().any(|node| node.after.iter().any(|on| doomed.contains(&on.workflow)))
            });
        if waited {
            return Err(CommandError::Project {
                message: "work in another project waits on this project's work".into(),
            });
        }
        self.projects.remove(&project);
        for id in &doomed {
            self.workflows.remove(id);
        }
        Ok(doomed)
    }

    fn new_project_id(&self) -> ProjectId {
        ProjectId(self.next_project + 1)
    }

    pub(crate) fn workflow(&mut self, workflow: WorkflowId) -> Result<&mut Workflow, CommandError> {
        self.workflows.get_mut(&workflow).ok_or(CommandError::UnknownWorkflow { workflow })
    }

    /// The id the next workflow gets.
    pub(crate) fn next_id(&self) -> WorkflowId {
        WorkflowId(self.next_workflow + 1)
    }

    /// Adds a workflow built for [`State::next_id`], such as one from a commander's plan.
    pub(crate) fn insert(&mut self, workflow: Workflow) -> &Workflow {
        debug_assert_eq!(workflow.id, self.next_id());
        self.next_workflow += 1;
        self.workflows.entry(workflow.id).or_insert(workflow)
    }
}

fn project_refused(error: ProjectError) -> CommandError {
    CommandError::Project { message: error.to_string() }
}

/// A trimmed name, refusing a blank one.
fn project_name(name: String) -> Result<String, CommandError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CommandError::Project { message: "a project needs a name".into() });
    }
    Ok(name.into())
}

fn refused(error: WorkflowError) -> CommandError {
    CommandError::Workflow { message: error.to_string() }
}

#[cfg(test)]
mod tests {
    use maestro_core::{Host, NodeDraft, NodeId, NodeOrigin};

    use super::*;

    fn draft(title: &str) -> NodeDraft {
        NodeDraft {
            title: title.into(),
            instructions: String::new(),
            model: None,
            difficulty: None,
            tags: Vec::new(),
            origin: NodeOrigin::User,
        }
    }

    #[test]
    fn edits_emit_the_new_graph_and_failures_emit_nothing() {
        let mut state = State::default();
        let (created, events) =
            state.apply(Command::CreateWorkflow { title: "t".into(), project: None });
        assert_eq!(created, Ok(Reply::WorkflowCreated { workflow: WorkflowId(1) }));
        assert_eq!(events.len(), 1);

        let workflow = WorkflowId(1);
        state.apply(Command::AddNode { workflow, node: draft("a") }).0.unwrap();
        state.apply(Command::AddNode { workflow, node: draft("b") }).0.unwrap();
        let (_, events) =
            state.apply(Command::AddEdge { workflow, from: NodeId(1), to: NodeId(2) });
        let [Event::WorkflowChanged { workflow: graph }] = &events[..] else {
            panic!("{events:?}")
        };
        assert_eq!(graph.edges().count(), 1);

        let (result, events) =
            state.apply(Command::AddEdge { workflow, from: NodeId(2), to: NodeId(1) });
        assert!(matches!(result, Err(CommandError::Workflow { .. })));
        assert!(events.is_empty());

        // A node waits only on a node of an earlier workflow that exists.
        state.apply(Command::CreateWorkflow { title: "later".into(), project: None }).0.unwrap();
        let on = NodeRef { workflow, node: NodeId(2) };
        let later = WorkflowId(2);
        state.apply(Command::AddNode { workflow: later, node: draft("c") }).0.unwrap();
        let wait = |on| Command::WaitFor { workflow: later, node: NodeId(1), on };
        assert!(state.apply(wait(NodeRef { workflow, node: NodeId(9) })).0.is_err());
        assert!(state.apply(Command::WaitFor { workflow, node: NodeId(1), on }).0.is_err());
        let (result, events) = state.apply(wait(on));
        assert_eq!((result, events.len()), (Ok(Reply::Done), 1));
        assert!(state.contains(on) && state.status(on) == Some(NodeStatus::Pending));

        let (result, _) = state.apply(Command::GetWorkflow { workflow: WorkflowId(5) });
        assert_eq!(result, Err(CommandError::UnknownWorkflow { workflow: WorkflowId(5) }));
    }

    #[test]
    fn projects_hold_resources_and_tell_their_commander_about_them() {
        let mut state = State::default();
        let blank = state.apply(Command::CreateProject { name: " ".into(), resources: Vec::new() });
        assert!(matches!(blank.0, Err(CommandError::Project { .. })));
        let folder = Resource::Directory { host: Host::Local, path: "/w".into(), base: "".into() };
        let (created, events) = state
            .apply(Command::CreateProject { name: "Site".into(), resources: vec![folder.clone()] });
        assert_eq!(created, Ok(Reply::ProjectCreated { project: ProjectId(1) }));
        assert!(matches!(&events[..], [Event::ProjectChanged { .. }]));
        let project = ProjectId(1);
        // A refused edit changes nothing.
        let again = state.apply(Command::AddResource { project, resource: folder });
        assert!(again.0.is_err() && again.1.is_empty());
        let machine = Resource::Machine { destination: "gpu".into() };
        let added = state.apply(Command::AddResource { project, resource: machine }).0;
        assert_eq!(added, Ok(Reply::ResourceAdded { resource: maestro_core::ResourceId(2) }));

        state
            .apply(Command::CreateWorkflow { title: "w".into(), project: Some(project) })
            .0
            .unwrap();
        let elsewhere = state.apply(Command::CreateWorkflow { title: "x".into(), project: None });
        elsewhere.0.unwrap();
        let briefing = state.briefing(project).unwrap();
        assert!(briefing.contains("- r1: folder /w") && briefing.contains("- r2: SSH machine gpu"));
        assert!(briefing.contains("wf1 \"w\"") && !briefing.contains("wf2"), "{briefing}");

        assert_eq!(state.delete_project(project), Ok(vec![WorkflowId(1)]));
        assert_eq!(state.workflows().len(), 1);
        let gone = state.apply(Command::RenameProject { project, name: "n".into() });
        assert_eq!(gone.0, Err(CommandError::UnknownProject { project }));
    }

    #[test]
    fn work_from_before_projects_moves_into_one() {
        let mut flow = Workflow::new(WorkflowId(1), "old");
        flow.target =
            Some(maestro_core::WorkflowTarget::beside(Host::Local, "/src/app".into(), "main"));
        let state = State::restore(Vec::new(), vec![flow], &[AgentId(3)]);
        let [project] = &state.projects()[..] else { panic!() };
        assert_eq!((project.name.as_str(), project.commander), ("app", Some(AgentId(3))));
        assert_eq!(project.resources.len(), 1);
        assert_eq!(state.workflows()[0].project, Some(project.id));
        // Nothing to move, nothing made.
        assert!(State::restore(Vec::new(), Vec::new(), &[]).projects().is_empty());
    }
}
