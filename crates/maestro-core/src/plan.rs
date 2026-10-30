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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::id::WorkflowId;
use crate::routing::Difficulty;
use crate::workflow::{
    NodeDraft, NodeOrigin, NodeRef, NodeStatus, Workflow, WorkflowError, WorkflowTarget,
};

/// The fence a plan is written in.
pub const PLAN_FENCE: &str = "maestro-plan";

/// What the commander is told before the user's first message.
pub const COMMANDER_INSTRUCTIONS: &str = r#"You are Maestro's commander. You talk with the user, understand what they want, and plan the work; you never do the work yourself. You must not edit files or run commands that change anything. Reading the repository to plan well is fine.

Workers carry out the plan. Each node of a plan goes to one worker, which runs in its own git worktree with the work of the nodes it depends on already merged in. Workers see only their node's instructions, so make each one complete on its own: what to change, where, and how to know it is done. Prefer a few substantial nodes to many tiny ones, and make nodes independent where you can so they run in parallel.

Rate each node's difficulty, which decides the model and effort it gets: "trivial" (mechanical), "easy" (small and well specified), "moderate" (ordinary feature or bug work), "hard" (design, many files, subtle behaviour) or "expert" (open-ended design, an unexplained bug).

Discuss the plan with the user first. Once they agree, include it in your reply exactly once, as a fenced block:

```maestro-plan
{"title": "Short name of the whole job",
 "nodes": [
   {"key": "api", "title": "Add the endpoint", "instructions": "...", "difficulty": "moderate", "depends_on": []},
   {"key": "ui", "title": "Show it in the UI", "instructions": "...", "difficulty": "easy", "depends_on": ["api"]}
 ]}
```

Keys are short names used only to express dependencies within the plan.

Work runs in the background, and the user keeps talking with you while it does, so they may ask for more before earlier work is finished. A message may begin with a <maestro-status> block that Maestro adds: it lists the work already planned, each step with its id (such as wf1/n2) and where it is. The user does not see it. When a new step needs a listed step's result (it changes the same code, or builds on what that step adds), put that step's id in the new step's "after" list, as in "after": ["wf1/n2"]. Maestro then starts the new step by itself once those steps have succeeded, with their work merged into its worktree. Leave "after" out for work that does not need them, so that it starts at once and runs in parallel.

A plan starts running as soon as you write it, so write the block only once the user agrees. The user can still edit the workflow while it runs. When asked to change a plan that has not started, write the whole revised plan as a new block."#;

/// What every worker is told, ahead of its node's instructions.
pub const WORKER_INSTRUCTIONS: &str = "You are a Maestro worker carrying out one step of a larger plan. Work only inside the current directory, which is a git worktree made for this step; the work of the steps this one depends on is already merged into it. Do not commit, push, or switch branches: Maestro commits your work when you finish. When you are done, reply with a short summary of what you changed and anything the user should check.";

/// A plan as the commander writes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// A name for the whole job.
    pub title: String,
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
    /// Keys of the steps that must succeed first.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Steps of work planned earlier, written `wf1/n2`, that must succeed first.
    #[serde(default)]
    pub after: Vec<String>,
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

    /// The plan as a workflow, every node marked as the commander's. `exists` says whether a
    /// node of another workflow, named in a step's `after`, is there to wait on.
    pub fn into_workflow(
        self,
        id: WorkflowId,
        target: Option<WorkflowTarget>,
        exists: impl Fn(NodeRef) -> bool,
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
                origin: NodeOrigin::Commander,
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
        report.push_str(&format!("{} {:?} ({state})\n", flow.id, flow.title));
        for node in flow.nodes() {
            let status = match node.status {
                NodeStatus::Pending => "waiting",
                NodeStatus::Running => "running",
                NodeStatus::AwaitingUser => "waiting for the user",
                NodeStatus::Succeeded => "succeeded",
                NodeStatus::Failed => "failed",
            };
            let reference = NodeRef { workflow: flow.id, node: node.id };
            report.push_str(&format!("- {reference} {:?}: {status}", node.title));
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
        let workflow = plan.clone().into_workflow(WorkflowId(1), None, |_| false).unwrap();
        assert_eq!(workflow.topological_order(), [NodeId(1), NodeId(2)]);
        assert_eq!(workflow.node(NodeId(2)).unwrap().difficulty, Some(Difficulty::Easy));
        assert!(workflow.nodes().all(|node| node.origin == NodeOrigin::Commander));
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
            depends_on: depends.iter().map(|&key| key.to_owned()).collect(),
            after: Vec::new(),
        };
        let plan = |nodes| Plan { title: "t".into(), nodes };
        let id = WorkflowId(1);
        assert!(matches!(
            plan(vec![node("a", &["b"]), node("b", &["a"])]).into_workflow(id, None, |_| false),
            Err(PlanError::Cycle(_))
        ));
        assert_eq!(
            plan(vec![node("a", &["z"])]).into_workflow(id, None, |_| false),
            Err(PlanError::UnknownDependency { step: "a".into(), missing: "z".into() })
        );
        assert_eq!(
            plan(vec![node("a", &[]), node("a", &[])]).into_workflow(id, None, |_| false),
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
            depends_on: Vec::new(),
            after: vec![after.into()],
        };
        let plan = |after| Plan { title: "t".into(), nodes: vec![step(after)] };
        let exists = |other| other == earlier;
        let flow = plan("wf1/n2").into_workflow(WorkflowId(2), None, exists).unwrap();
        assert_eq!(flow.node(NodeId(1)).unwrap().after, [earlier]);
        // Not there, not an id, and not earlier: the plan's own workflow is wf2.
        for wrong in ["wf1/n3", "the api step", "wf2/n1"] {
            assert_eq!(
                plan(wrong).into_workflow(WorkflowId(2), None, |other| other.node != NodeId(3)),
                Err(PlanError::UnknownWork { step: "a".into(), missing: wrong.into() })
            );
        }
    }

    #[test]
    fn the_status_report_names_every_step_and_what_it_waits_on() {
        assert_eq!(status_report([]), None);
        let plan = |nodes| Plan { title: "Job".into(), nodes };
        let node = |key: &str, depends: &[&str], after: &[&str]| PlanNode {
            key: key.into(),
            title: key.to_uppercase(),
            instructions: String::new(),
            difficulty: None,
            depends_on: depends.iter().map(|&key| key.to_owned()).collect(),
            after: after.iter().map(|&key| key.to_owned()).collect(),
        };
        let mut first =
            plan(vec![node("a", &[], &[])]).into_workflow(WorkflowId(1), None, |_| false).unwrap();
        first.started = true;
        let second = plan(vec![node("b", &[], &["wf1/n1"]), node("c", &["b"], &[])])
            .into_workflow(WorkflowId(2), None, |_| true)
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
}
