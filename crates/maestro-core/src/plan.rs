//! The commander's plans, and the standing instructions for both roles.
//!
//! The commander is an ordinary CLI agent with no tools that write. It plans in conversation and,
//! when the user agrees, writes the plan as a fenced `maestro-plan` block of JSON in its reply
//! ([`COMMANDER_INSTRUCTIONS`] tells it how). The daemon finds the blocks with [`Plan::extract`]
//! and turns each into a [`Workflow`] with [`Plan::into_workflow`], which the user can then edit
//! before running it. Text is the interface because every one of the four CLIs can produce it;
//! a tool protocol would need a different bridge for each.
//!
//! Work goes on while the user and the commander talk, and the user may ask for more before
//! earlier work is done. So each message to the commander starts with a [`status_report`] of the
//! work Maestro has, and a plan's step can wait on steps listed there (`after`). Maestro starts
//! it by itself once they have succeeded.
//!
//! A step too big for one worker can be delegated (`"delegate": true`): a lead, started with
//! [`LEAD_INSTRUCTIONS`], plans that step alone in the same format, and its plan becomes a
//! workflow under the step. A lead may delegate in turn, so plans nest as deep as the work does.
//!
//! Every handover loses detail, so the commander's default is not to split at all: one step that
//! inherits the conversation (`"context": "conversation"`). Maestro hands its worker what the
//! user and the planner said, as [`conversation`] renders it, alongside the step's instructions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::AgentEvent;
use crate::id::{ResourceId, WorkflowId};
use crate::routing::Difficulty;
use crate::workflow::{
    NodeDraft, NodeOrigin, NodeRef, NodeStatus, Workflow, WorkflowError, WorkflowTarget,
};

/// The fence a plan is written in.
pub const PLAN_FENCE: &str = "maestro-plan";

/// What the commander is told before the user's first message.
pub const COMMANDER_INSTRUCTIONS: &str = r#"You are Maestro's commander. You talk with the user, understand what they want, and plan the work; you never do the work yourself. You must not edit files or run commands that change anything. Reading the repository to plan well is fine.

Workers carry out the plan. Each node of a plan goes to one worker, which runs in its own git worktree with the work of the nodes it depends on already merged in. A worker sees its node's instructions and the replies of the workers before it, so make each node's instructions complete on its own: what to change, where, and how to know it is done.

Every split loses detail, so most jobs should be a single node with "context": "conversation". Maestro then hands that worker this conversation with the user as well as the node's instructions, and one worker with the whole picture does the job. Split the work into several nodes only when the parts are independent enough to run in parallel and each needs little of the rest, when they work in different repositories, or when the job is too large for one worker to hold. Cut such nodes where little context crosses between them, not by kind of work, and give a node "context": "conversation" whenever this conversation matters to it.

Rate each node's difficulty, which decides the model and effort it gets: "trivial" (mechanical), "easy" (small and well specified), "moderate" (ordinary feature or bug work), "hard" (design, many files, subtle behaviour) or "expert" (open-ended design, an unexplained bug). Tag each node with one to three short lowercase words for the kind of work, such as "ui", "api", "tests", "docs" or "refactor"; Maestro learns which models do well at which kind. Reuse the same words for the same kind of work.

Discuss the plan with the user first. Once they agree, include it in your reply exactly once, as a fenced block:

```maestro-plan
{"title": "Short name of the whole job",
 "resource": "r1",
 "nodes": [
   {"key": "api", "title": "Add the endpoint", "instructions": "...", "difficulty": "moderate", "tags": ["api"], "depends_on": [], "context": "conversation"},
   {"key": "ui", "title": "Show it in the UI", "instructions": "...", "difficulty": "easy", "tags": ["ui"], "depends_on": ["api"]}
 ]}
```

Keys are short names used only to express dependencies within the plan. The example has two nodes to show dependencies; a single node is the usual plan.

A node too large or too open for one worker can be handed to a lead instead: add "delegate": true and describe the outcome wanted rather than the steps. A lead is an agent like you that plans that node alone as a plan of its own, with workers (or leads of its own) under it, and the node succeeds once the lead's plan has. Delegate only what genuinely needs planning of its own; most nodes should go straight to a worker.

A worker does its node itself: Maestro stops it if it hands the work to sub-agents of its own (its CLI's built-in task or subagent tools). When a node is easier for a worker that may use them, for example to search a large codebase in parallel, add "subagents": true. A lead may delegate in turn; add "subagents": false to a delegated node to keep its lead from delegating further, so that its plan goes straight to workers.

You lead one project. A <maestro-project> block, which Maestro adds ahead of a message the first time and whenever it changes, lists the project's resources with their ids: GitHub repositories, folders on a machine, SSH machines, and the AI providers workers may use. Decide which repository or folder each plan works in and put its id in the plan as "resource": "r1". A GitHub repository is cloned onto a machine first: add "machine": "r3" to run it on an SSH machine of the project, or leave it out for the daemon's own machine. A folder always runs on its own machine. When the project has a single repository you may leave "resource" out. When it is not clear which resource the user means, or the choice matters (a slow machine, a repository they might not want touched), say which you would use and why, and ask before writing the plan. One plan works in one repository; for work in several, write one plan for each. If the project lacks something the work needs, tell the user to add it to the project.

Work runs in the background, and the user keeps talking with you while it does, so they may ask for more before earlier work is finished. A message may begin with a <maestro-status> block that Maestro adds: it lists the work already planned, each step with its id (such as wf1/n2) and where it is. The user does not see it. When a new step needs a listed step's result (it changes the same code, or builds on what that step adds), put that step's id in the new step's "after" list, as in "after": ["wf1/n2"]. Maestro then starts the new step by itself once those steps have succeeded, with their work merged into its worktree. Leave "after" out for work that does not need them, so that it starts at once and runs in parallel.

A plan starts running as soon as you write it, so write the block only once the user agrees. The user can still edit the workflow while it runs. When asked to change a plan that has not started, write the whole revised plan as a new block."#;

/// What a lead is told before the step it plans.
pub const LEAD_INSTRUCTIONS: &str = r#"You are a Maestro lead. A step of a larger plan was handed to you because it needs planning of its own: you plan it as a plan of smaller steps, and workers carry them out. You never do the work yourself. You must not edit files or run commands that change anything. Reading the repository, which is checked out here with the work of the steps before yours merged in, to plan well is fine.

Each node of your plan goes to one worker, which runs in its own git worktree starting from this one, with the work of the nodes it depends on already merged in. A worker sees its node's instructions and the replies of the workers before it, so make each node's instructions complete on its own: what to change, where, and how to know it is done. Every split loses detail: split only where the parts are independent enough to run in parallel, cut nodes where little context crosses between them, and prefer one substantial node to several that need each other's details. Add "context": "conversation" to a node to hand its worker what was said to you here, the step you were given and anything the user told you, as well as its instructions. A node that itself needs planning of its own can go to a lead under you: add "delegate": true and describe the outcome wanted. A worker does its node itself and is stopped if it hands the work to sub-agents of its own; add "subagents": true to a node whose worker may use them, or "subagents": false to a delegated node whose lead must not delegate further.

Rate each node's difficulty: "trivial", "easy", "moderate", "hard" or "expert"; it decides the model and effort the node gets. Tag each node with one to three short lowercase words for the kind of work, such as "ui", "api", "tests", "docs" or "refactor".

Write the plan in your reply exactly once, as a fenced block:

```maestro-plan
{"title": "Short name of this step's work",
 "nodes": [
   {"key": "model", "title": "Add the data model", "instructions": "...", "difficulty": "moderate", "tags": ["api"], "depends_on": [], "context": "conversation"},
   {"key": "tests", "title": "Test it", "instructions": "...", "difficulty": "easy", "tags": ["tests"], "depends_on": ["model"]}
 ]}
```

Keys are short names used only to express dependencies within the plan. Your plan works in the same repository as the step you were given, so it names no resource. It starts running as soon as you write it, and your step succeeds once every node of it has. If the step cannot be planned without an answer from the user, ask your question instead of writing a plan; the user answers you directly. When the user asks you for changes later, write a plan for just the extra work."#;

/// What every worker is told, ahead of its node's instructions.
pub const WORKER_INSTRUCTIONS: &str = "You are a Maestro worker carrying out one step of a larger plan. Work only inside the current directory, which is a git worktree made for this step; the work of the steps this one depends on is already merged into it. Do not commit, push, or switch branches: Maestro commits your work when you finish. When you are done, reply with a short summary of what you changed and anything the user should check.";

/// What a worker that may not use sub-agents is told after [`WORKER_INSTRUCTIONS`].
pub const NO_SUBAGENTS: &str = "Do this step yourself. Do not hand it, or any part of it, to sub-agents, subagent tools or task tools: Maestro stops a worker that does.";

/// What a lead that may not delegate is told after its step.
pub const NO_DELEGATION: &str = "Do not delegate: no node of your plan may have \"delegate\": true. Every node goes straight to a worker.";

/// A plan as the commander writes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// A name for the whole job.
    pub title: String,
    /// The project's repository or folder it works in, by resource id (`r1`).
    #[serde(default)]
    pub resource: Option<String>,
    /// The project's SSH machine a GitHub repository is cloned onto and worked on.
    #[serde(default)]
    pub machine: Option<String>,
    /// Its steps.
    pub nodes: Vec<PlanNode>,
}

/// One step of a plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanNode {
    /// A name used only for `depends_on`.
    pub key: String,
    /// A short name for the graph view.
    pub title: String,
    /// What the worker is asked to do.
    pub instructions: String,
    /// How hard it is.
    pub difficulty: Option<Difficulty>,
    /// The kind of work it is.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Keys of the steps that must succeed first.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Steps of work planned earlier, written `wf1/n2`, that must succeed first.
    #[serde(default)]
    pub after: Vec<String>,
    /// Hand the step to a lead, who plans it as a workflow of its own.
    #[serde(default)]
    pub delegate: bool,
    /// Whether the step's agent may hand work to sub-agents of its own, or `None` for the
    /// default of its role: a lead may delegate, a worker may not use its CLI's sub-agents.
    #[serde(default)]
    pub subagents: Option<bool>,
    /// What the step's agent is handed besides its instructions.
    #[serde(default)]
    pub context: PlanContext,
}

/// What a step's agent is handed besides its instructions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanContext {
    /// Its instructions alone.
    #[default]
    Instructions,
    /// The planner's conversation, as [`conversation`] renders it, as well.
    Conversation,
}

/// Why a plan could not become a workflow.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PlanError {
    /// The block was not valid plan JSON.
    #[error("the plan is not valid: {0}")]
    Malformed(String),
    /// Two steps share a key.
    #[error("two steps are both called {0:?}")]
    DuplicateKey(String),
    /// A step depends on a key no step has.
    #[error("step {step:?} depends on {missing:?}, which is not in the plan")]
    UnknownDependency {
        /// The step.
        step: String,
        /// The key it named.
        missing: String,
    },
    /// A step waits on earlier work that does not exist.
    #[error("step {step:?} waits on {missing:?}, which is not a step of any earlier workflow")]
    UnknownWork {
        /// The step.
        step: String,
        /// What it named.
        missing: String,
    },
    /// The dependencies loop.
    #[error("the plan's dependencies loop: {0}")]
    Cycle(WorkflowError),
    /// `resource` or `machine` is not a resource id.
    #[error("{0:?} is not a resource id like r1")]
    NotAResource(String),
}

impl Plan {
    /// Every `maestro-plan` block in a reply, in order, each parsed or with its error.
    pub fn extract(text: &str) -> Vec<Result<Plan, PlanError>> {
        let opening = format!("```{PLAN_FENCE}");
        let mut plans = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find(&opening) {
            let body = &rest[start + opening.len()..];
            let Some(end) = body.find("```") else { break };
            plans.push(
                serde_json::from_str(body[..end].trim())
                    .map_err(|error| PlanError::Malformed(error.to_string())),
            );
            rest = &body[end + 3..];
        }
        plans
    }

    /// The resource ids the plan names for its repository and its machine.
    pub fn resources(&self) -> Result<(Option<ResourceId>, Option<ResourceId>), PlanError> {
        let parse = |field: &Option<String>| match field.as_deref().map(str::trim) {
            None | Some("") => Ok(None),
            Some(text) => ResourceId::parse(text)
                .map(Some)
                .ok_or_else(|| PlanError::NotAResource(text.into())),
        };
        Ok((parse(&self.resource)?, parse(&self.machine)?))
    }

    /// The plan as a workflow, every node marked as the commander's. `exists` says whether a
    /// node of another workflow, named in a step's `after`, is there to wait on, and
    /// `conversation` is what a step that asks for the conversation is handed.
    pub fn into_workflow(
        self,
        id: WorkflowId,
        target: Option<WorkflowTarget>,
        exists: impl Fn(NodeRef) -> bool,
        conversation: Option<&str>,
    ) -> Result<Workflow, PlanError> {
        let mut workflow = Workflow::new(id, self.title);
        workflow.target = target;
        let mut ids = BTreeMap::new();
        for node in &self.nodes {
            let draft = NodeDraft {
                title: node.title.clone(),
                instructions: node.instructions.clone(),
                model: None,
                difficulty: node.difficulty,
                tags: node.tags.clone(),
                delegate: node.delegate,
                subagents: node.subagents,
                origin: NodeOrigin::Commander,
                conversation: match node.context {
                    PlanContext::Conversation => conversation.map(str::to_owned),
                    PlanContext::Instructions => None,
                },
            };
            if ids.insert(node.key.clone(), workflow.add_node(draft)).is_some() {
                return Err(PlanError::DuplicateKey(node.key.clone()));
            }
        }
        for node in &self.nodes {
            for dependency in &node.depends_on {
                let from = *ids.get(dependency).ok_or_else(|| PlanError::UnknownDependency {
                    step: node.key.clone(),
                    missing: dependency.clone(),
                })?;
                workflow.add_edge(from, ids[&node.key]).map_err(PlanError::Cycle)?;
            }
            for other in &node.after {
                let unknown =
                    || PlanError::UnknownWork { step: node.key.clone(), missing: other.clone() };
                let on = other.parse::<NodeRef>().map_err(|()| unknown())?;
                if !exists(on) {
                    return Err(unknown());
                }
                workflow.wait_for(ids[&node.key], on).map_err(|_| unknown())?;
            }
        }
        Ok(workflow)
    }
}

/// What Maestro says to a planner when a plan of its reply could not be used.
pub const PLAN_REJECTED: &str = "Maestro could not use that plan";

/// How long a rendered conversation may be, in characters; older messages are left out first.
pub const CONVERSATION_LIMIT: usize = 24_000;

/// A planner's conversation as a worker reads it: each message to the planner and each of its
/// replies, oldest first, without the plans it wrote (the worker has its own step's) or Maestro's
/// notes about plans it could not use. When it is longer than `limit` characters, the oldest
/// messages are left out, and it says so. `None` when nothing was said.
pub fn conversation(transcript: &[AgentEvent], limit: usize) -> Option<String> {
    let mut messages = Vec::new();
    for event in transcript {
        let (who, text) = match event {
            AgentEvent::Prompted { text } if !text.starts_with(PLAN_REJECTED) => ("Message", text),
            AgentEvent::TurnCompleted { answer } => ("Reply", answer),
            _ => continue,
        };
        let text = without_plans(text);
        let text = text.trim();
        if !text.is_empty() {
            messages.push(format!("## {who}\n\n{text}"));
        }
    }
    let mut kept = Vec::new();
    let mut length = 0;
    for message in messages.iter().rev() {
        let size = message.chars().count() + 2;
        if length + size > limit && !kept.is_empty() {
            break;
        }
        length += size;
        kept.push(message.as_str());
    }
    if kept.is_empty() {
        return None;
    }
    let left_out = messages.len() - kept.len();
    kept.reverse();
    let mut text = kept.join("\n\n");
    if text.chars().count() > limit {
        // A single message longer than the limit: keep its end, the newest part.
        let skip = text.chars().count() - limit;
        text = text.chars().skip(skip).collect();
    }
    if left_out > 0 {
        text = format!("({left_out} earlier messages left out)\n\n{text}");
    }
    Some(text)
}

/// `text` with every `maestro-plan` block replaced by a note that a plan was written there.
fn without_plans(text: &str) -> String {
    let opening = format!("```{PLAN_FENCE}");
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(&opening) {
        out.push_str(&rest[..start]);
        let body = &rest[start + opening.len()..];
        out.push_str("(a plan was written here)");
        match body.find("```") {
            Some(end) => rest = &body[end + 3..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// How many workflows a status report lists at most, newest first.
const REPORTED_WORKFLOWS: usize = 20;

/// The work Maestro has, as the commander reads it ahead of the user's message, or `None` when
/// there is none. Lists the newest workflows, each step with its id and where it is.
pub fn status_report<'a>(workflows: impl IntoIterator<Item = &'a Workflow>) -> Option<String> {
    let mut workflows: Vec<&Workflow> = workflows.into_iter().collect();
    if workflows.is_empty() {
        return None;
    }
    workflows.sort_by_key(|flow| std::cmp::Reverse(flow.id));
    let mut report = String::from("<maestro-status>\n");
    for flow in workflows.into_iter().take(REPORTED_WORKFLOWS) {
        let done = flow.nodes().all(|node| node.status == NodeStatus::Succeeded);
        let state = match (flow.started, done) {
            (_, true) => "done",
            (true, false) => "running",
            (false, false) => "not started",
        };
        report.push_str(&format!("{} {:?} ({state})", flow.id, flow.title));
        if let Some(parent) = flow.parent {
            report.push_str(&format!(", planned by a lead for {parent}"));
        }
        report.push('\n');
        for node in flow.nodes() {
            let status = match node.status {
                NodeStatus::Pending => "waiting",
                NodeStatus::Running => "running",
                NodeStatus::AwaitingUser => "waiting for the user",
                NodeStatus::AwaitingChoice => "waiting for the user to pick one model's work",
                NodeStatus::Succeeded => "succeeded",
                NodeStatus::Failed => "failed",
            };
            let reference = NodeRef { workflow: flow.id, node: node.id };
            report.push_str(&format!("- {reference} {:?}: {status}", node.title));
            if node.delegate {
                report.push_str(", delegated to a lead");
            }
            let waits: Vec<String> = flow
                .dependencies(node.id)
                .map(|dep| NodeRef { workflow: flow.id, node: dep }.to_string())
                .chain(node.after.iter().map(NodeRef::to_string))
                .collect();
            if !waits.is_empty() {
                report.push_str(&format!(", after {}", waits.join(", ")));
            }
            report.push('\n');
        }
    }
    report.push_str("</maestro-status>");
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::NodeId;

    #[test]
    fn the_example_in_the_instructions_is_a_valid_plan() {
        let plans = Plan::extract(COMMANDER_INSTRUCTIONS);
        let [Ok(plan)] = &plans[..] else { panic!("{plans:?}") };
        let workflow = plan.clone().into_workflow(WorkflowId(1), None, |_| false, None).unwrap();
        assert_eq!(workflow.topological_order(), [NodeId(1), NodeId(2)]);
        assert_eq!(workflow.node(NodeId(2)).unwrap().difficulty, Some(Difficulty::Easy));
        assert_eq!(workflow.node(NodeId(2)).unwrap().tags, ["ui"]);
        assert!(workflow.nodes().all(|node| node.origin == NodeOrigin::Commander));
    }

    #[test]
    fn the_example_in_the_lead_instructions_is_a_valid_plan() {
        let plans = Plan::extract(LEAD_INSTRUCTIONS);
        let [Ok(plan)] = &plans[..] else { panic!("{plans:?}") };
        assert_eq!(plan.resources(), Ok((None, None)));
        let workflow = plan.clone().into_workflow(WorkflowId(1), None, |_| false, None).unwrap();
        assert_eq!(workflow.topological_order(), [NodeId(1), NodeId(2)]);
    }

    #[test]
    fn a_step_can_be_delegated() {
        let text = "```maestro-plan\n{\"title\":\"t\",\"nodes\":[{\"key\":\"a\",\"title\":\"A\",\"instructions\":\"\",\"difficulty\":\"hard\",\"delegate\":true},{\"key\":\"b\",\"title\":\"B\",\"instructions\":\"\",\"difficulty\":\"easy\"}]}\n```";
        let [Ok(plan)] = &Plan::extract(text)[..] else { panic!() };
        let workflow = plan.clone().into_workflow(WorkflowId(1), None, |_| false, None).unwrap();
        assert!(workflow.node(NodeId(1)).unwrap().delegate);
        assert!(!workflow.node(NodeId(2)).unwrap().delegate);
    }

    #[test]
    fn leads_may_delegate_and_workers_may_not_use_subagents_unless_the_plan_says() {
        let text = "```maestro-plan\n{\"title\":\"t\",\"nodes\":[\
            {\"key\":\"a\",\"title\":\"A\",\"instructions\":\"\",\"difficulty\":\"hard\",\"delegate\":true},\
            {\"key\":\"b\",\"title\":\"B\",\"instructions\":\"\",\"difficulty\":\"hard\",\"delegate\":true,\"subagents\":false},\
            {\"key\":\"c\",\"title\":\"C\",\"instructions\":\"\",\"difficulty\":\"easy\"},\
            {\"key\":\"d\",\"title\":\"D\",\"instructions\":\"\",\"difficulty\":\"easy\",\"subagents\":true}]}\n```";
        let [Ok(plan)] = &Plan::extract(text)[..] else { panic!() };
        let workflow = plan.clone().into_workflow(WorkflowId(1), None, |_| false, None).unwrap();
        let allows = |node| workflow.node(NodeId(node)).unwrap().allows_subagents();
        assert_eq!([allows(1), allows(2), allows(3), allows(4)], [true, false, false, true]);
    }

    #[test]
    fn finds_every_block_and_reports_bad_ones() {
        let text = "Here it is:\n```maestro-plan\n{\"title\":\"a\",\"nodes\":[]}\n```\nand\n```maestro-plan\nnot json\n```\n```rust\nfn main() {}\n```";
        let plans = Plan::extract(text);
        assert_eq!(plans.len(), 2);
        assert!(plans[0].is_ok());
        assert!(matches!(plans[1], Err(PlanError::Malformed(_))));
        assert!(Plan::extract("no plan here").is_empty());
    }

    #[test]
    fn rejects_plans_that_cannot_be_ordered() {
        let node = |key: &str, depends: &[&str]| PlanNode {
            key: key.into(),
            title: key.into(),
            instructions: String::new(),
            difficulty: None,
            tags: Vec::new(),
            depends_on: depends.iter().map(|&key| key.to_owned()).collect(),
            after: Vec::new(),
            delegate: false,
            subagents: None,
            context: PlanContext::Instructions,
        };
        let plan = |nodes| Plan { title: "t".into(), resource: None, machine: None, nodes };
        let id = WorkflowId(1);
        assert!(matches!(
            plan(vec![node("a", &["b"]), node("b", &["a"])]).into_workflow(
                id,
                None,
                |_| false,
                None
            ),
            Err(PlanError::Cycle(_))
        ));
        assert_eq!(
            plan(vec![node("a", &["z"])]).into_workflow(id, None, |_| false, None),
            Err(PlanError::UnknownDependency { step: "a".into(), missing: "z".into() })
        );
        assert_eq!(
            plan(vec![node("a", &[]), node("a", &[])]).into_workflow(id, None, |_| false, None),
            Err(PlanError::DuplicateKey("a".into()))
        );
    }

    #[test]
    fn a_step_waits_on_earlier_work_that_exists() {
        let earlier = NodeRef { workflow: WorkflowId(1), node: NodeId(2) };
        let step = |after: &str| PlanNode {
            key: "a".into(),
            title: "a".into(),
            instructions: String::new(),
            difficulty: None,
            tags: Vec::new(),
            depends_on: Vec::new(),
            after: vec![after.into()],
            delegate: false,
            subagents: None,
            context: PlanContext::Instructions,
        };
        let plan = |after| Plan {
            title: "t".into(),
            resource: None,
            machine: None,
            nodes: vec![step(after)],
        };
        let exists = |other| other == earlier;
        let flow = plan("wf1/n2").into_workflow(WorkflowId(2), None, exists, None).unwrap();
        assert_eq!(flow.node(NodeId(1)).unwrap().after, [earlier]);
        // Not there, not an id, and not earlier: the plan's own workflow is wf2.
        for wrong in ["wf1/n3", "the api step", "wf2/n1"] {
            assert_eq!(
                plan(wrong).into_workflow(
                    WorkflowId(2),
                    None,
                    |other| other.node != NodeId(3),
                    None
                ),
                Err(PlanError::UnknownWork { step: "a".into(), missing: wrong.into() })
            );
        }
    }

    #[test]
    fn the_status_report_names_every_step_and_what_it_waits_on() {
        assert_eq!(status_report([]), None);
        let plan = |nodes| Plan { title: "Job".into(), resource: None, machine: None, nodes };
        let node = |key: &str, depends: &[&str], after: &[&str]| PlanNode {
            key: key.into(),
            title: key.to_uppercase(),
            instructions: String::new(),
            difficulty: None,
            tags: Vec::new(),
            depends_on: depends.iter().map(|&key| key.to_owned()).collect(),
            after: after.iter().map(|&key| key.to_owned()).collect(),
            delegate: false,
            subagents: None,
            context: PlanContext::Instructions,
        };
        let mut first = plan(vec![node("a", &[], &[])])
            .into_workflow(WorkflowId(1), None, |_| false, None)
            .unwrap();
        first.started = true;
        let second = plan(vec![node("b", &[], &["wf1/n1"]), node("c", &["b"], &[])])
            .into_workflow(WorkflowId(2), None, |_| true, None)
            .unwrap();
        let report = status_report([&first, &second]).unwrap();
        assert_eq!(
            report,
            "<maestro-status>\n\
             wf2 \"Job\" (not started)\n\
             - wf2/n1 \"B\": waiting, after wf1/n1\n\
             - wf2/n2 \"C\": waiting, after wf2/n1\n\
             wf1 \"Job\" (running)\n\
             - wf1/n1 \"A\": waiting\n\
             </maestro-status>"
        );
    }

    #[test]
    fn a_step_can_inherit_the_conversation() {
        let text = "```maestro-plan\n{\"title\":\"t\",\"nodes\":[{\"key\":\"a\",\"title\":\"A\",\"instructions\":\"\",\"difficulty\":\"easy\",\"context\":\"conversation\"},{\"key\":\"b\",\"title\":\"B\",\"instructions\":\"\",\"difficulty\":\"easy\"}]}\n```";
        let [Ok(plan)] = &Plan::extract(text)[..] else { panic!() };
        assert_eq!(plan.nodes[0].context, PlanContext::Conversation);
        assert_eq!(plan.nodes[1].context, PlanContext::Instructions);
        let workflow =
            plan.clone().into_workflow(WorkflowId(1), None, |_| false, Some("said")).unwrap();
        assert_eq!(workflow.node(NodeId(1)).unwrap().conversation.as_deref(), Some("said"));
        assert_eq!(workflow.node(NodeId(2)).unwrap().conversation, None);
    }

    #[test]
    fn the_conversation_leaves_out_plans_and_maestros_notes() {
        let transcript = [
            AgentEvent::Started,
            AgentEvent::Prompted { text: "Add dark mode, but keep the logo blue.".into() },
            AgentEvent::Text { text: "partial".into() },
            AgentEvent::TurnCompleted { answer: "Shall I?".into() },
            AgentEvent::Prompted { text: "Yes.".into() },
            AgentEvent::TurnCompleted {
                answer: "Here it is:\n```maestro-plan\n{}\n```\nStarted.".into(),
            },
            AgentEvent::Prompted { text: format!("{PLAN_REJECTED}: bad. Please write it again.") },
        ];
        assert_eq!(
            conversation(&transcript, CONVERSATION_LIMIT).unwrap(),
            "## Message\n\nAdd dark mode, but keep the logo blue.\n\n## Reply\n\nShall I?\n\n\
             ## Message\n\nYes.\n\n## Reply\n\nHere it is:\n(a plan was written here)\nStarted."
        );
        assert_eq!(conversation(&[AgentEvent::Started], CONVERSATION_LIMIT), None);
    }

    #[test]
    fn a_long_conversation_keeps_its_newest_messages() {
        let transcript: Vec<AgentEvent> = (0..10)
            .map(|index| AgentEvent::Prompted {
                text: format!("message {index} {}", "x".repeat(20)),
            })
            .collect();
        let text = conversation(&transcript, 100).unwrap();
        assert!(text.starts_with("(8 earlier messages left out)"), "{text}");
        assert!(text.contains("message 8") && text.ends_with(&"x".repeat(20)));
        let one = [AgentEvent::Prompted { text: "y".repeat(500) }];
        assert_eq!(conversation(&one, 100).unwrap().chars().count(), 100);
    }
}
