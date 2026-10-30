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
id!(
    /// One project: the resources a commander may use, and the work planned with them.
    ProjectId,
    "p"
);
id!(
    /// One resource of a project, unique within that project.
    ResourceId,
    "r"
);

impl ResourceId {
    /// Reads an id as the commander writes it, `r2`, or as a bare number.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        text.strip_prefix('r').unwrap_or(text).parse().ok().map(Self)
    }
}
