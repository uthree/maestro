//! The graph of work the commander plans and the user can edit.
//!
//! A workflow is a directed acyclic graph: each [`Node`] is one piece of work for one worker, and
//! an [`Edge`] from `a` to `b` means `b` starts only once `a` has succeeded. The commander writes
//! the first version; after that the user and the commander edit the same graph through the same
//! operations, and every operation keeps it acyclic so the order of work is always defined.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::path::PathBuf;

use crate::id::{AgentId, NodeId, WorkflowId};
use crate::provider::ModelChoice;
use crate::routing::Difficulty;
use crate::target::Host;

/// Who put a node into the graph, shown so the user can tell their own edits from the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeOrigin {
    /// Planned by the commander.
    Commander,
    /// Added or rewritten by the user.
    User,
}

/// Where a node is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// Waiting for its dependencies, or for the workflow to start.
    Pending,
    /// A worker is on it.
    Running,
    /// The worker is waiting for the user to answer.
    AwaitingUser,
    /// Finished and accepted.
    Succeeded,
    /// Finished without success; its dependents do not start.
    Failed,
}

/// The parts of a node a caller supplies; the workflow assigns the id and the status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDraft {
    /// A short name for the graph view.
    pub title: String,
    /// What the worker is asked to do.
    pub instructions: String,
    /// The model chosen for it, or `None` to let the router choose when it runs.
    pub model: Option<ModelChoice>,
    /// How hard it is; decides the model's grade and effort when the router chooses.
    #[serde(default)]
    pub difficulty: Option<Difficulty>,
    /// Who wrote it.
    pub origin: NodeOrigin,
}

/// Changes to a node the user or the commander makes after it was added. `None` leaves a field
/// as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeEdit {
    /// A new title.
    pub title: Option<String>,
    /// New instructions.
    pub instructions: Option<String>,
    /// A new model; `Some(None)` hands the choice back to the router.
    pub model: Option<Option<ModelChoice>>,
    /// A new difficulty.
    pub difficulty: Option<Difficulty>,
}

/// One piece of work for one worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Its id within the workflow.
    pub id: NodeId,
    /// A short name for the graph view.
    pub title: String,
    /// What the worker is asked to do.
    pub instructions: String,
    /// The model chosen for it, if one has been.
    pub model: Option<ModelChoice>,
    /// How hard it is.
    #[serde(default)]
    pub difficulty: Option<Difficulty>,
    /// Who wrote it.
    pub origin: NodeOrigin,
    /// Where it is in its life.
    pub status: NodeStatus,
    /// The worker on it, once one has started. A user's message to the node goes to this agent.
    #[serde(default)]
    pub agent: Option<AgentId>,
    /// The branch its work is committed on, once it has a worktree.
    #[serde(default)]
    pub branch: Option<String>,
}

/// The repository a workflow's workers check out, and what happens to their work.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkflowTarget {
    /// The machine the repository is on.
    pub host: Host,
    /// The repository's main checkout.
    pub repository: PathBuf,
    /// The branch every node's branch starts from, and the pull request merges into.
    pub base: String,
    /// Where the nodes' worktrees go; each gets a directory named after its branch.
    pub worktrees: PathBuf,
    /// Open a pull request for the whole workflow once every node has succeeded.
    pub publish: bool,
}

/// `to` depends on `from`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Edge {
    /// The node that must succeed first.
    pub from: NodeId,
    /// The node that waits for it.
    pub to: NodeId,
}

/// Why an edit to a workflow was refused. The workflow is unchanged after any of these.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum WorkflowError {
    /// The node is not in this workflow.
    #[error("node {0} is not in this workflow")]
    UnknownNode(NodeId),
    /// A node cannot depend on itself.
    #[error("node {0} cannot depend on itself")]
    SelfDependency(NodeId),
    /// The edge would close a loop, so no order of work would exist.
    #[error("{from} → {to} would make a cycle")]
    Cycle {
        /// Proposed dependency.
        from: NodeId,
        /// Proposed dependent.
        to: NodeId,
    },
}

/// A directed acyclic graph of work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "WorkflowRepr", into = "WorkflowRepr")]
pub struct Workflow {
    /// Its id.
    pub id: WorkflowId,
    /// A short name for lists.
    pub title: String,
    /// Where its workers run; a workflow without one can be drawn and edited, not run.
    pub target: Option<WorkflowTarget>,
    nodes: BTreeMap<NodeId, Node>,
    edges: BTreeSet<Edge>,
    next_node: u64,
}

/// How a workflow travels: nodes as a list rather than a map keyed by id. Integer map keys
/// become strings in JSON, which serde cannot turn back into integers once the workflow sits
/// inside an internally tagged message.
#[derive(Serialize, Deserialize)]
struct WorkflowRepr {
    id: WorkflowId,
    title: String,
    #[serde(default)]
    target: Option<WorkflowTarget>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    next_node: u64,
}

impl From<WorkflowRepr> for Workflow {
    fn from(repr: WorkflowRepr) -> Self {
        Self {
            id: repr.id,
            title: repr.title,
            target: repr.target,
            nodes: repr.nodes.into_iter().map(|node| (node.id, node)).collect(),
            edges: repr.edges.into_iter().collect(),
            next_node: repr.next_node,
        }
    }
}

impl From<Workflow> for WorkflowRepr {
    fn from(flow: Workflow) -> Self {
        Self {
            id: flow.id,
            title: flow.title,
            target: flow.target,
            nodes: flow.nodes.into_values().collect(),
            edges: flow.edges.into_iter().collect(),
            next_node: flow.next_node,
        }
    }
}

impl Workflow {
    /// An empty workflow.
    pub fn new(id: WorkflowId, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            target: None,
            nodes: BTreeMap::new(),
            edges: BTreeSet::new(),
            next_node: 1,
        }
    }

    /// Every node, in id order.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    /// Every edge, sorted.
    pub fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.edges.iter()
    }

    /// One node.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Adds a node with no dependencies and returns its id.
    pub fn add_node(&mut self, draft: NodeDraft) -> NodeId {
        let id = NodeId(self.next_node);
        self.next_node += 1;
        let NodeDraft { title, instructions, model, difficulty, origin } = draft;
        let status = NodeStatus::Pending;
        let node = Node {
            id,
            title,
            instructions,
            model,
            difficulty,
            origin,
            status,
            agent: None,
            branch: None,
        };
        self.nodes.insert(id, node);
        id
    }

    /// Removes a node and every edge touching it.
    pub fn remove_node(&mut self, id: NodeId) -> Result<Node, WorkflowError> {
        let node = self.nodes.remove(&id).ok_or(WorkflowError::UnknownNode(id))?;
        self.edges.retain(|edge| edge.from != id && edge.to != id);
        Ok(node)
    }

    /// Makes `to` wait for `from`. Adding an edge that already exists is not an error.
    pub fn add_edge(&mut self, from: NodeId, to: NodeId) -> Result<(), WorkflowError> {
        self.require(from)?;
        self.require(to)?;
        if from == to {
            return Err(WorkflowError::SelfDependency(from));
        }
        // The new edge closes a loop exactly when `from` is already reachable from `to`.
        if self.reaches(to, from) {
            return Err(WorkflowError::Cycle { from, to });
        }
        self.edges.insert(Edge { from, to });
        Ok(())
    }

    /// Removes a dependency. Returns whether it existed.
    pub fn remove_edge(&mut self, from: NodeId, to: NodeId) -> bool {
        self.edges.remove(&Edge { from, to })
    }

    /// Sets a node's status.
    pub fn set_status(&mut self, id: NodeId, status: NodeStatus) -> Result<(), WorkflowError> {
        self.node_mut(id)?.status = status;
        Ok(())
    }

    /// Changes a node's content. The user's edit of a commander's node makes it the user's.
    pub fn edit_node(
        &mut self,
        id: NodeId,
        edit: NodeEdit,
        by: NodeOrigin,
    ) -> Result<(), WorkflowError> {
        let node = self.node_mut(id)?;
        let NodeEdit { title, instructions, model, difficulty } = edit;
        if let Some(title) = title {
            node.title = title;
        }
        if let Some(instructions) = instructions {
            node.instructions = instructions;
        }
        if let Some(model) = model {
            node.model = model;
        }
        if let Some(difficulty) = difficulty {
            node.difficulty = Some(difficulty);
        }
        if by == NodeOrigin::User {
            node.origin = NodeOrigin::User;
        }
        Ok(())
    }

    /// Records the worker, model and branch a node runs with.
    pub fn assign(
        &mut self,
        id: NodeId,
        agent: AgentId,
        model: ModelChoice,
        branch: String,
    ) -> Result<(), WorkflowError> {
        let node = self.node_mut(id)?;
        node.agent = Some(agent);
        node.model = Some(model);
        node.branch = Some(branch);
        node.status = NodeStatus::Running;
        Ok(())
    }

    /// The node a worker is on.
    pub fn node_of(&self, agent: AgentId) -> Option<NodeId> {
        self.nodes.values().find(|node| node.agent == Some(agent)).map(|node| node.id)
    }

    fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, WorkflowError> {
        self.nodes.get_mut(&id).ok_or(WorkflowError::UnknownNode(id))
    }

    /// The nodes `id` waits for.
    pub fn dependencies(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.edges.iter().filter(move |edge| edge.to == id).map(|edge| edge.from)
    }

    /// Pending nodes whose dependencies have all succeeded: the work that can start now.
    pub fn ready(&self) -> Vec<NodeId> {
        self.nodes
            .values()
            .filter(|node| node.status == NodeStatus::Pending)
            .filter(|node| {
                self.dependencies(node.id)
                    .all(|dep| self.nodes[&dep].status == NodeStatus::Succeeded)
            })
            .map(|node| node.id)
            .collect()
    }

    /// Every node, each after all of its dependencies; ties broken by id so the order is stable.
    pub fn topological_order(&self) -> Vec<NodeId> {
        let mut waiting: BTreeMap<NodeId, usize> = self.nodes.keys().map(|&id| (id, 0)).collect();
        for edge in &self.edges {
            *waiting.get_mut(&edge.to).expect("edges only join existing nodes") += 1;
        }
        let mut free: VecDeque<NodeId> =
            waiting.iter().filter(|(_, count)| **count == 0).map(|(&id, _)| id).collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(id) = free.pop_front() {
            order.push(id);
            for edge in self.edges.iter().filter(|edge| edge.from == id) {
                let count = waiting.get_mut(&edge.to).expect("edges only join existing nodes");
                *count -= 1;
                if *count == 0 {
                    free.push_back(edge.to);
                }
            }
        }
        order
    }

    fn require(&self, id: NodeId) -> Result<(), WorkflowError> {
        if self.nodes.contains_key(&id) { Ok(()) } else { Err(WorkflowError::UnknownNode(id)) }
    }

    fn reaches(&self, start: NodeId, goal: NodeId) -> bool {
        let mut seen = BTreeSet::new();
        let mut stack = vec![start];
        while let Some(id) = stack.pop() {
            if id == goal {
                return true;
            }
            if seen.insert(id) {
                stack.extend(self.edges.iter().filter(|edge| edge.from == id).map(|edge| edge.to));
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(title: &str) -> NodeDraft {
        NodeDraft {
            title: title.into(),
            instructions: String::new(),
            model: None,
            difficulty: None,
            origin: NodeOrigin::Commander,
        }
    }

    fn diamond() -> (Workflow, [NodeId; 4]) {
        let mut flow = Workflow::new(WorkflowId(1), "diamond");
        let a = flow.add_node(draft("a"));
        let b = flow.add_node(draft("b"));
        let c = flow.add_node(draft("c"));
        let d = flow.add_node(draft("d"));
        for (from, to) in [(a, b), (a, c), (b, d), (c, d)] {
            flow.add_edge(from, to).unwrap();
        }
        (flow, [a, b, c, d])
    }

    #[test]
    fn refuses_cycles_and_self_loops_without_changing_the_graph() {
        let (mut flow, [a, _, _, d]) = diamond();
        let before = flow.clone();
        assert_eq!(flow.add_edge(d, a), Err(WorkflowError::Cycle { from: d, to: a }));
        assert_eq!(flow.add_edge(a, a), Err(WorkflowError::SelfDependency(a)));
        assert_eq!(flow.add_edge(a, NodeId(99)), Err(WorkflowError::UnknownNode(NodeId(99))));
        assert_eq!(flow, before);
    }

    #[test]
    fn orders_every_node_after_its_dependencies() {
        let (flow, [a, b, c, d]) = diamond();
        assert_eq!(flow.topological_order(), vec![a, b, c, d]);
    }

    #[test]
    fn ready_follows_success_but_not_failure() {
        let (mut flow, [a, b, c, d]) = diamond();
        assert_eq!(flow.ready(), vec![a]);
        flow.set_status(a, NodeStatus::Succeeded).unwrap();
        assert_eq!(flow.ready(), vec![b, c]);
        flow.set_status(b, NodeStatus::Succeeded).unwrap();
        flow.set_status(c, NodeStatus::Failed).unwrap();
        assert!(flow.ready().is_empty());
        flow.set_status(c, NodeStatus::Succeeded).unwrap();
        assert_eq!(flow.ready(), vec![d]);
    }

    #[test]
    fn removing_a_node_removes_its_edges_and_ids_are_not_reused() {
        let (mut flow, [a, b, c, d]) = diamond();
        flow.remove_node(b).unwrap();
        assert_eq!(flow.dependencies(d).collect::<Vec<_>>(), vec![c]);
        assert_eq!(flow.edges().count(), 2);
        assert_ne!(flow.add_node(draft("e")), b);
        assert!(flow.node(a).is_some());
    }

    #[test]
    fn round_trips_through_json() {
        let (flow, _) = diamond();
        let text = serde_json::to_string(&flow).unwrap();
        assert_eq!(serde_json::from_str::<Workflow>(&text).unwrap(), flow);
    }
}
