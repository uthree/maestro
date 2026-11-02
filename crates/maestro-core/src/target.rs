//! Where a worker's process runs and which checkout it works in.
//!
//! The machine a person sits at and the machine that does the work are usually different: the
//! frontend runs on a laptop, the daemon and every worker on a build server reached over SSH. A
//! [`Host`] names the machine *from the daemon's point of view*, so [`Host::Local`] on a server
//! daemon means the server.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A machine a worker can run on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Host {
    /// The machine the daemon runs on.
    Local,
    /// Another machine, reached with the system `ssh` so the user's config, keys and agent apply.
    Ssh {
        /// Anything `ssh` accepts as a destination: `user@host`, or a `Host` alias from config.
        destination: String,
    },
    /// A WSL distribution on the daemon's machine, which must be Windows. Commands run in it
    /// with `wsl.exe`, so paths are the distribution's own (`/home/me/app`, not `C:\…`).
    Wsl {
        /// The distribution's name, as `wsl --list` shows it; blank for the default one.
        #[serde(default)]
        distribution: String,
    },
    /// A running Docker container on the daemon's machine, entered with `docker exec`. Paths are
    /// inside the container, and the container must have `sh`, `git` and the agent CLIs.
    Docker {
        /// The container's name or id.
        container: String,
    },
}

impl Host {
    /// How the commander and logs name it, in English.
    pub fn describe(&self) -> String {
        match self {
            Self::Local => "the daemon's machine".into(),
            Self::Ssh { destination } => format!("SSH machine {destination}"),
            Self::Wsl { distribution } if distribution.is_empty() => {
                "the default WSL distribution".into()
            }
            Self::Wsl { distribution } => format!("WSL distribution {distribution}"),
            Self::Docker { container } => format!("Docker container {container}"),
        }
    }
}

/// A git worktree created for one worker, so parallel workers never share a checkout.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Worktree {
    /// Branch the worktree checks out; created from `base` when missing.
    pub branch: String,
    /// Branch or commit the new branch starts from.
    pub base: String,
    /// The worktree's directory on the host.
    pub path: PathBuf,
}

/// A repository on a host, optionally narrowed to a worktree of it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Workspace {
    /// The machine.
    pub host: Host,
    /// The repository's main checkout on that machine.
    pub repository: PathBuf,
    /// The worker's own worktree, when it has one.
    pub worktree: Option<Worktree>,
}

impl Workspace {
    /// The directory an agent process is started in.
    pub fn working_directory(&self) -> &PathBuf {
        self.worktree.as_ref().map_or(&self.repository, |tree| &tree.path)
    }
}
