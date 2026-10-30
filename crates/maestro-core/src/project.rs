//! Projects: what one commander may work with.
//!
//! A project is the user's name for a body of work, like a project in ChatGPT's desktop app. It
//! holds [`Resource`]s: GitHub repositories, folders on a machine, SSH machines, and the AI
//! providers its workers may run on. Each project has one commander, which is told the resources
//! at the start of its conversation and whenever they change ([`Project::briefing`]), and decides
//! which to use for each plan, asking the user when the choice is unclear. A plan names its
//! repository and machine by resource id (`"resource": "r1"`), and [`Project::place`] turns that
//! into where the work runs.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::id::{AgentId, ProjectId, ResourceId};
use crate::provider::ProviderKind;
use crate::target::Host;

/// Something a project's work may use.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Resource {
    /// A repository on GitHub, cloned onto the machine a plan runs on the first time it is used.
    GitHub {
        /// `owner/name`.
        repository: String,
        /// The branch work starts from and pull requests merge into.
        base: String,
    },
    /// A folder on a machine; a git checkout, for work to run in it.
    Directory {
        /// The machine, from the daemon's point of view.
        host: Host,
        /// The folder on that machine.
        path: PathBuf,
        /// The branch work starts from and pull requests merge into.
        base: String,
    },
    /// A machine reached with the system `ssh`, where GitHub repositories can be cloned and run.
    Machine {
        /// Anything `ssh` accepts as a destination.
        destination: String,
    },
    /// An AI provider the project's workers may run on. A project with none may use any.
    Provider {
        /// The CLI.
        provider: ProviderKind,
    },
}

impl Resource {
    /// A GitHub repository from `owner/name` or its URL, or `None` if it is neither.
    pub fn github(text: &str, base: impl Into<String>) -> Option<Self> {
        let text = text.trim().trim_end_matches('/');
        let text = text.strip_suffix(".git").unwrap_or(text);
        let text = ["https://github.com/", "http://github.com/", "github.com/", "git@github.com:"]
            .iter()
            .find_map(|prefix| text.strip_prefix(prefix))
            .unwrap_or(text);
        let (owner, name) = text.split_once('/')?;
        let valid = |part: &str| {
            !part.is_empty()
                && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        (valid(owner) && valid(name))
            .then(|| Self::GitHub { repository: format!("{owner}/{name}"), base: base.into() })
    }

    /// Whether work can run in it: a repository or a folder.
    pub fn is_repository(&self) -> bool {
        matches!(self, Self::GitHub { .. } | Self::Directory { .. })
    }

    /// How the commander reads it.
    fn describe(&self) -> String {
        match self {
            Self::GitHub { repository, base } => format!(
                "GitHub repository {repository} (base branch {base}), cloned onto the machine the plan runs on"
            ),
            Self::Directory { host, path, base } => {
                format!("folder {} on {} (base branch {base})", path.display(), describe(host))
            }
            Self::Machine { destination } => format!("SSH machine {destination}"),
            Self::Provider { provider } => format!("AI provider {}", provider.label()),
        }
    }
}

fn describe(host: &Host) -> String {
    match host {
        Host::Local => "the daemon's machine".into(),
        Host::Ssh { destination } => format!("SSH machine {destination}"),
    }
}

/// A resource with the id the project gave it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectResource {
    /// Its id within the project.
    pub id: ResourceId,
    /// What it is.
    pub resource: Resource,
}

/// One project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Its id.
    pub id: ProjectId,
    /// Its name.
    pub name: String,
    /// Its resources, in the order they were added.
    pub resources: Vec<ProjectResource>,
    /// Its commander: the one running, or the last one, whose conversation is the project's.
    pub commander: Option<AgentId>,
    next_resource: u64,
}

/// Where a plan's work runs, before any repository is cloned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The machine.
    pub host: Host,
    /// The checkout on it.
    pub checkout: Checkout,
    /// The branch work starts from.
    pub base: String,
}

/// The checkout a plan's work runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checkout {
    /// A folder that is already there.
    Path(PathBuf),
    /// A clone of a GitHub repository, `owner/name`, made on the machine if it is missing.
    Clone(String),
}

/// Why a project refused an edit or a choice of resources.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ProjectError {
    /// The project already has that resource.
    #[error("the project already has {0}")]
    Duplicate(String),
    /// A resource is incomplete.
    #[error("{0}")]
    Invalid(&'static str),
    /// No resource has that id.
    #[error("the project has no resource {0}")]
    Unknown(String),
    /// The resource named is not of the kind needed there.
    #[error("{id} is not {wanted}")]
    WrongKind {
        /// What was named.
        id: ResourceId,
        /// What was needed.
        wanted: &'static str,
    },
    /// Several repositories could be meant.
    #[error(
        "the project has several repositories and folders, so the plan must say which with \"resource\""
    )]
    Ambiguous,
    /// A folder is on one machine and the plan named another.
    #[error("{0} is on another machine than the one the plan names")]
    Elsewhere(ResourceId),
    /// The project does not allow that provider.
    #[error("the project does not allow {0}")]
    NotAllowed(&'static str),
}

impl Project {
    /// A project with no resources.
    pub fn new(id: ProjectId, name: impl Into<String>) -> Self {
        Self { id, name: name.into(), resources: Vec::new(), commander: None, next_resource: 1 }
    }

    /// One resource.
    pub fn resource(&self, id: ResourceId) -> Option<&Resource> {
        self.resources.iter().find(|entry| entry.id == id).map(|entry| &entry.resource)
    }

    /// Adds a resource and returns its id. Refuses one the project has or one with empty fields.
    pub fn add(&mut self, resource: Resource) -> Result<ResourceId, ProjectError> {
        let resource = match resource {
            Resource::GitHub { repository, base } => Resource::github(&repository, branch(base))
                .ok_or(ProjectError::Invalid("a GitHub repository is written owner/name"))?,
            Resource::Directory { host, path, base } => {
                if path.as_os_str().is_empty() {
                    return Err(ProjectError::Invalid("a folder needs a path"));
                }
                Resource::Directory { host: machine(host)?, path, base: branch(base) }
            }
            Resource::Machine { destination } => {
                let Host::Ssh { destination } = machine(Host::Ssh { destination })? else {
                    return Err(ProjectError::Invalid("a machine needs an SSH destination"));
                };
                Resource::Machine { destination }
            }
            provider @ Resource::Provider { .. } => provider,
        };
        if self.resources.iter().any(|entry| same(&entry.resource, &resource)) {
            return Err(ProjectError::Duplicate(resource.describe()));
        }
        let id = ResourceId(self.next_resource);
        self.next_resource += 1;
        self.resources.push(ProjectResource { id, resource });
        Ok(id)
    }

    /// Removes a resource. Returns whether it was there.
    pub fn remove(&mut self, id: ResourceId) -> bool {
        let before = self.resources.len();
        self.resources.retain(|entry| entry.id != id);
        self.resources.len() != before
    }

    /// The providers the project allows, or `None` when it allows any.
    pub fn providers(&self) -> Option<Vec<ProviderKind>> {
        let allowed: Vec<_> = self
            .resources
            .iter()
            .filter_map(|entry| match entry.resource {
                Resource::Provider { provider } => Some(provider),
                _ => None,
            })
            .collect();
        (!allowed.is_empty()).then_some(allowed)
    }

    /// Whether the project's work may run on `provider`.
    pub fn allows(&self, provider: ProviderKind) -> bool {
        self.providers().is_none_or(|allowed| allowed.contains(&provider))
    }

    /// Keeps the providers of `available` the project allows.
    pub fn allowed(&self, available: &[ProviderKind]) -> Vec<ProviderKind> {
        available.iter().copied().filter(|&provider| self.allows(provider)).collect()
    }

    /// Refuses `provider` unless the project allows it.
    pub fn check(&self, provider: ProviderKind) -> Result<(), ProjectError> {
        if self.allows(provider) { Ok(()) } else { Err(ProjectError::NotAllowed(provider.label())) }
    }

    /// The folder the commander reads to plan: the first folder among the resources.
    pub fn reading_room(&self) -> Option<(Host, PathBuf)> {
        self.resources.iter().find_map(|entry| match &entry.resource {
            Resource::Directory { host, path, .. } => Some((host.clone(), path.clone())),
            _ => None,
        })
    }

    /// Where a plan runs, from the repository or folder and the machine it names. A plan that
    /// names no repository uses the project's only one; with none, it is `None`, a sketch to edit.
    pub fn place(
        &self,
        resource: Option<ResourceId>,
        machine: Option<ResourceId>,
    ) -> Result<Option<Placement>, ProjectError> {
        let host = match machine {
            None => None,
            Some(id) => match self.resource(id) {
                Some(Resource::Machine { destination }) => {
                    Some(Host::Ssh { destination: destination.clone() })
                }
                Some(_) => return Err(ProjectError::WrongKind { id, wanted: "a machine" }),
                None => return Err(ProjectError::Unknown(id.to_string())),
            },
        };
        let id = match resource {
            Some(id) => id,
            None => {
                let mut repositories =
                    self.resources.iter().filter(|entry| entry.resource.is_repository());
                match (repositories.next(), repositories.next()) {
                    (None, _) => return Ok(None),
                    (Some(only), None) => only.id,
                    (Some(_), Some(_)) => return Err(ProjectError::Ambiguous),
                }
            }
        };
        match self.resource(id) {
            Some(Resource::GitHub { repository, base }) => Ok(Some(Placement {
                host: host.unwrap_or(Host::Local),
                checkout: Checkout::Clone(repository.clone()),
                base: base.clone(),
            })),
            Some(Resource::Directory { host: there, path, base }) => {
                if host.as_ref().is_some_and(|host| host != there) {
                    return Err(ProjectError::Elsewhere(id));
                }
                Ok(Some(Placement {
                    host: there.clone(),
                    checkout: Checkout::Path(path.clone()),
                    base: base.clone(),
                }))
            }
            Some(_) => Err(ProjectError::WrongKind { id, wanted: "a repository or a folder" }),
            None => Err(ProjectError::Unknown(id.to_string())),
        }
    }

    /// What the commander is told about the project, ahead of the user's message.
    pub fn briefing(&self) -> String {
        let mut text = format!("<maestro-project name={:?}>\n", self.name);
        let mut providers = Vec::new();
        for entry in &self.resources {
            match entry.resource {
                Resource::Provider { provider } => providers.push(provider.label()),
                ref resource => {
                    text.push_str(&format!("- {}: {}\n", entry.id, resource.describe()))
                }
            }
        }
        if !self.resources.iter().any(|entry| entry.resource.is_repository()) {
            text.push_str("- No repository or folder yet: plans can be drawn but not run.\n");
        }
        if providers.is_empty() {
            text.push_str("Workers may run on any installed AI provider.\n");
        } else {
            text.push_str(&format!("Workers may run only on: {}.\n", providers.join(", ")));
        }
        text.push_str("</maestro-project>");
        text
    }
}

/// `base`, or `main` when it is blank.
fn branch(base: String) -> String {
    let base = base.trim();
    if base.is_empty() { "main".into() } else { base.into() }
}

/// `host` with a trimmed destination, refusing a blank one.
fn machine(host: Host) -> Result<Host, ProjectError> {
    match host {
        Host::Local => Ok(Host::Local),
        Host::Ssh { destination } => {
            let destination = destination.trim();
            if destination.is_empty() || destination.contains(char::is_whitespace) {
                return Err(ProjectError::Invalid(
                    "an SSH destination is one word, like user@host",
                ));
            }
            Ok(Host::Ssh { destination: destination.into() })
        }
    }
}

/// Whether two resources are the same thing, whatever their branches.
fn same(a: &Resource, b: &Resource) -> bool {
    match (a, b) {
        (Resource::GitHub { repository: a, .. }, Resource::GitHub { repository: b, .. }) => {
            a.eq_ignore_ascii_case(b)
        }
        (
            Resource::Directory { host: a, path: p, .. },
            Resource::Directory { host: b, path: q, .. },
        ) => a == b && p == q,
        (a, b) => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(destination: &str) -> Host {
        Host::Ssh { destination: destination.into() }
    }

    #[test]
    fn github_repositories_are_read_from_names_and_urls() {
        let name = |text| match Resource::github(text, "main") {
            Some(Resource::GitHub { repository, .. }) => Some(repository),
            _ => None,
        };
        assert_eq!(name("uthree/maestro").as_deref(), Some("uthree/maestro"));
        assert_eq!(
            name("https://github.com/uthree/maestro.git").as_deref(),
            Some("uthree/maestro")
        );
        assert_eq!(name("git@github.com:uthree/maestro").as_deref(), Some("uthree/maestro"));
        assert_eq!(name("maestro"), None);
        assert_eq!(name("a/b/c"), None);
    }

    #[test]
    fn resources_are_checked_and_never_added_twice() {
        let mut project = Project::new(ProjectId(1), "p");
        let folder = |path: &str| Resource::Directory {
            host: Host::Local,
            path: path.into(),
            base: " ".into(),
        };
        let first = project.add(folder("/src/app")).unwrap();
        assert_eq!(first, ResourceId(1));
        assert!(matches!(project.add(folder("/src/app")), Err(ProjectError::Duplicate(_))));
        assert!(project.add(folder("")).is_err());
        assert!(project.add(Resource::Machine { destination: "a b".into() }).is_err());
        let github = Resource::GitHub { repository: "Uthree/Maestro".into(), base: "dev".into() };
        project.add(github).unwrap();
        let again = Resource::GitHub { repository: "uthree/maestro".into(), base: "main".into() };
        assert!(project.add(again).is_err());
        // A blank branch means main.
        assert_eq!(
            project.resource(first),
            Some(&Resource::Directory {
                host: Host::Local,
                path: "/src/app".into(),
                base: "main".into()
            })
        );
        assert!(project.remove(first) && !project.remove(first));
        // Ids are not reused.
        assert_eq!(project.add(folder("/src/app")).unwrap(), ResourceId(3));
    }

    #[test]
    fn providers_are_an_allow_list_once_there_is_one() {
        let mut project = Project::new(ProjectId(1), "p");
        assert!(project.allows(ProviderKind::Codex));
        project.add(Resource::Provider { provider: ProviderKind::ClaudeCode }).unwrap();
        assert!(!project.allows(ProviderKind::Codex));
        assert_eq!(project.allowed(&ProviderKind::ALL), [ProviderKind::ClaudeCode]);
        assert_eq!(project.check(ProviderKind::Codex), Err(ProjectError::NotAllowed("Codex")));
    }

    #[test]
    fn a_plan_is_placed_on_the_resources_it_names() {
        let mut project = Project::new(ProjectId(1), "p");
        assert_eq!(project.place(None, None), Ok(None));
        let folder = project
            .add(Resource::Directory { host: ssh("box"), path: "/w".into(), base: "main".into() })
            .unwrap();
        // The only repository is used without naming it.
        let placed = project.place(None, None).unwrap().unwrap();
        assert_eq!((placed.host, placed.checkout), (ssh("box"), Checkout::Path("/w".into())));

        let github =
            project.add(Resource::GitHub { repository: "o/r".into(), base: "dev".into() }).unwrap();
        let machine = project.add(Resource::Machine { destination: "gpu".into() }).unwrap();
        assert_eq!(project.place(None, None), Err(ProjectError::Ambiguous));
        let cloned = project.place(Some(github), Some(machine)).unwrap().unwrap();
        assert_eq!(cloned.host, ssh("gpu"));
        assert_eq!((cloned.checkout, cloned.base.as_str()), (Checkout::Clone("o/r".into()), "dev"));
        assert_eq!(project.place(Some(github), None).unwrap().unwrap().host, Host::Local);
        // A folder stays on its machine.
        assert_eq!(
            project.place(Some(folder), Some(machine)),
            Err(ProjectError::Elsewhere(folder))
        );
        assert!(matches!(project.place(Some(machine), None), Err(ProjectError::WrongKind { .. })));
        assert!(matches!(project.place(Some(ResourceId(9)), None), Err(ProjectError::Unknown(_))));
    }

    #[test]
    fn the_briefing_lists_resources_by_id() {
        let mut project = Project::new(ProjectId(1), "Site");
        project.add(Resource::GitHub { repository: "o/r".into(), base: "main".into() }).unwrap();
        project.add(Resource::Machine { destination: "gpu".into() }).unwrap();
        project.add(Resource::Provider { provider: ProviderKind::Codex }).unwrap();
        assert_eq!(
            project.briefing(),
            "<maestro-project name=\"Site\">\n\
             - r1: GitHub repository o/r (base branch main), cloned onto the machine the plan runs on\n\
             - r2: SSH machine gpu\n\
             Workers may run only on: Codex.\n\
             </maestro-project>"
        );
    }
}
