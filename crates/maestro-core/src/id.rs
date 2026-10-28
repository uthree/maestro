//! Identifiers handed out by the daemon.
//!
//! The daemon is the only place that allocates them, so a plain counter is unique for as long as
//! the daemon's state is, and the ids stay small and readable in logs and on the wire.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }
    };
}

id!(
    /// One workflow: the graph the commander planned for a single request.
    WorkflowId,
    "wf"
);
id!(
    /// One node of a workflow, unique within that workflow.
    NodeId,
    "n"
);
id!(
    /// One running agent process, commander or worker.
    AgentId,
    "agent"
);
