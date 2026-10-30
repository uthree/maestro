//! The commander's plans, and the standing instructions for both roles.
//!
//! The commander is an ordinary CLI agent with no tools that write. It plans in conversation and,
//! when the user agrees, writes the plan as a fenced `maestro-plan` block of JSON in its reply
//! ([`COMMANDER_INSTRUCTIONS`] tells it how). The daemon finds the blocks with [`Plan::extract`]
//! and turns each into a [`Workflow`] with [`Plan::into_workflow`], which the user can then edit
//! before running it. Text is the interface because every one of the four CLIs can produce it;
//! a tool protocol would need a different bridge for each.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::id::WorkflowId;
use crate::routing::Difficulty;
use crate::workflow::{NodeDraft, NodeOrigin, Workflow, WorkflowError, WorkflowTarget};

/// The fence a plan is written in.
pub const PLAN_FENCE: &str = "maestro-plan";

/// What the commander is told before the user's first message.
pub const COMMANDER_INSTRUCTIONS: &str = r#"You are Maestro's commander. You talk with the user, understand what they want, and plan the work; you never do the work yourself. You must not edit files or run commands that change anything. Reading the repository to plan well is fine.

Workers carry out the plan. Each node of a plan goes to one worker, which runs in its own git worktree with the work of the nodes it depends on already merged in. Workers see only their node's instructions, so make each one complete on its own: what to change, where, and how to know it is done. Prefer a few substantial nodes to many tiny ones, and make nodes independent where you can so they run in parallel.

Rate each node's difficulty, which decides the model and effort it gets: "trivial" (mechanical), "easy" (small and well specified), "moderate" (ordinary feature or bug work), "hard" (design, many files, subtle behaviour) or "expert" (open-ended design, an unexplained bug). Tag each node with one to three short lowercase words for the kind of work, such as "ui", "api", "tests", "docs" or "refactor"; Maestro learns which models do well at which kind. Reuse the same words for the same kind of work.

Discuss the plan with the user first. Once they agree, include it in your reply exactly once, as a fenced block:

```maestro-plan
{"title": "Short name of the whole job",
 "nodes": [
   {"key": "api", "title": "Add the endpoint", "instructions": "...", "difficulty": "moderate", "tags": ["api"], "depends_on": []},
   {"key": "ui", "title": "Show it in the UI", "instructions": "...", "difficulty": "easy", "tags": ["ui"], "depends_on": ["api"]}
 ]}
```

Keys are short names used only to express dependencies. The user can edit the plan before running it. When asked to change a plan, write the whole revised plan as a new block."#;

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
    /// The kind of work it is.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Keys of the steps that must succeed first.
    #[serde(default)]
    pub depends_on: Vec<String>,
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

    /// The plan as a workflow, every node marked as the commander's.
    pub fn into_workflow(
        self,
        id: WorkflowId,
        target: Option<WorkflowTarget>,
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
        }
        Ok(workflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::NodeId;

    #[test]
    fn the_example_in_the_instructions_is_a_valid_plan() {
        let plans = Plan::extract(COMMANDER_INSTRUCTIONS);
        let [Ok(plan)] = &plans[..] else { panic!("{plans:?}") };
        let workflow = plan.clone().into_workflow(WorkflowId(1), None).unwrap();
        assert_eq!(workflow.topological_order(), [NodeId(1), NodeId(2)]);
        assert_eq!(workflow.node(NodeId(2)).unwrap().difficulty, Some(Difficulty::Easy));
        assert_eq!(workflow.node(NodeId(2)).unwrap().tags, ["ui"]);
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
            tags: Vec::new(),
            depends_on: depends.iter().map(|&key| key.to_owned()).collect(),
        };
        let plan = |nodes| Plan { title: "t".into(), nodes };
        let id = WorkflowId(1);
        assert!(matches!(
            plan(vec![node("a", &["b"]), node("b", &["a"])]).into_workflow(id, None),
            Err(PlanError::Cycle(_))
        ));
        assert_eq!(
            plan(vec![node("a", &["z"])]).into_workflow(id, None),
            Err(PlanError::UnknownDependency { step: "a".into(), missing: "z".into() })
        );
        assert_eq!(
            plan(vec![node("a", &[]), node("a", &[])]).into_workflow(id, None),
            Err(PlanError::DuplicateKey("a".into()))
        );
    }
}
