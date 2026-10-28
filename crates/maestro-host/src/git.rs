//! The git operations a worker's life needs: a worktree of its own, commits, a push.
//!
//! Every function takes the [`Host`] the repository lives on, so the same code drives a checkout
//! on the daemon's machine and one on a server behind it.

use std::path::{Path, PathBuf};

use maestro_core::{Host, Worktree};

use crate::{HostCommand, HostError};

fn git(directory: &Path) -> HostCommand {
    HostCommand::new("git").arg("-C").arg(directory.to_string_lossy())
}

/// Creates the worktree, checking out `branch` if it exists and creating it from `base` if not.
pub fn add_worktree(host: &Host, repository: &Path, worktree: &Worktree) -> Result<(), HostError> {
    let exists = git(repository)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{}", worktree.branch))
        .run(host)
        .is_ok();
    let path = worktree.path.to_string_lossy();
    let command = git(repository).args(["worktree", "add"]);
    let command = if exists {
        command.arg(path).arg(&worktree.branch)
    } else {
        command.arg("-b").arg(&worktree.branch).arg(path).arg(&worktree.base)
    };
    command.run(host).map(drop)
}

/// Removes a worktree's directory and its registration. `force` discards uncommitted changes.
pub fn remove_worktree(
    host: &Host,
    repository: &Path,
    path: &Path,
    force: bool,
) -> Result<(), HostError> {
    let mut command = git(repository).args(["worktree", "remove"]);
    if force {
        command = command.arg("--force");
    }
    command.arg(path.to_string_lossy()).run(host).map(drop)
}

/// One entry of `git worktree list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// Its directory.
    pub path: PathBuf,
    /// The commit checked out.
    pub head: String,
    /// The branch checked out, or `None` when the head is detached.
    pub branch: Option<String>,
}

/// Every worktree of the repository, the main checkout first.
pub fn list_worktrees(host: &Host, repository: &Path) -> Result<Vec<WorktreeEntry>, HostError> {
    let text = git(repository).args(["worktree", "list", "--porcelain"]).run(host)?;
    Ok(parse_worktrees(&text))
}

fn parse_worktrees(text: &str) -> Vec<WorktreeEntry> {
    text.split("\n\n")
        .filter_map(|block| {
            let mut entry =
                WorktreeEntry { path: PathBuf::new(), head: String::new(), branch: None };
            for line in block.lines() {
                if let Some(path) = line.strip_prefix("worktree ") {
                    entry.path = path.into();
                } else if let Some(head) = line.strip_prefix("HEAD ") {
                    entry.head = head.into();
                } else if let Some(branch) = line.strip_prefix("branch ") {
                    entry.branch = Some(branch.trim_start_matches("refs/heads/").into());
                }
            }
            (!entry.path.as_os_str().is_empty()).then_some(entry)
        })
        .collect()
}

/// Stages everything and commits it. Returns the new commit, or `None` when nothing changed.
pub fn commit_all(
    host: &Host,
    directory: &Path,
    message: &str,
) -> Result<Option<String>, HostError> {
    git(directory).args(["add", "--all"]).run(host)?;
    if git(directory).args(["status", "--porcelain"]).run(host)?.trim().is_empty() {
        return Ok(None);
    }
    git(directory).args(["commit", "--quiet", "-m", message]).run(host)?;
    Ok(Some(git(directory).args(["rev-parse", "HEAD"]).run(host)?.trim().to_owned()))
}

/// Pushes `branch` to `origin` and sets it as the upstream.
pub fn push(host: &Host, directory: &Path, branch: &str) -> Result<(), HostError> {
    git(directory).args(["push", "--quiet", "-u", "origin", branch]).run(host).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let run =
            |args: &[&str]| git(directory.path()).args(args.iter().copied()).run(&Host::Local);
        run(&["init", "--quiet", "--initial-branch=main"]).unwrap();
        run(&["config", "user.email", "test@example.com"]).unwrap();
        run(&["config", "user.name", "Test"]).unwrap();
        run(&["commit", "--quiet", "--allow-empty", "-m", "root"]).unwrap();
        directory
    }

    #[test]
    fn a_worker_gets_its_own_worktree_and_commits_in_it() {
        let repo = repository();
        let trees = tempfile::tempdir().unwrap();
        let worktree = Worktree {
            branch: "maestro/n1".into(),
            base: "main".into(),
            path: trees.path().join("n1"),
        };
        add_worktree(&Host::Local, repo.path(), &worktree).unwrap();

        let listed = list_worktrees(&Host::Local, repo.path()).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[1].branch.as_deref(), Some("maestro/n1"));

        assert_eq!(commit_all(&Host::Local, &worktree.path, "nothing").unwrap(), None);
        std::fs::write(worktree.path.join("a.txt"), "a").unwrap();
        let commit = commit_all(&Host::Local, &worktree.path, "add a").unwrap().unwrap();
        assert_eq!(commit.len(), 40);

        // Removing and re-adding reuses the branch, keeping the work.
        remove_worktree(&Host::Local, repo.path(), &worktree.path, false).unwrap();
        add_worktree(&Host::Local, repo.path(), &worktree).unwrap();
        assert!(worktree.path.join("a.txt").exists());
    }

    #[test]
    fn parses_detached_and_branch_entries() {
        let text =
            "worktree /r\nHEAD aaa\nbranch refs/heads/main\n\nworktree /t\nHEAD bbb\ndetached\n";
        let entries = parse_worktrees(text);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(
            entries[1],
            WorktreeEntry { path: "/t".into(), head: "bbb".into(), branch: None }
        );
    }
}
