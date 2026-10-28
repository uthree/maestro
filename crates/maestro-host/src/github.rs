//! Pull requests, through the `gh` CLI signed in on the host that holds the checkout.

use std::path::Path;

use maestro_core::Host;
use serde_json::Value;

use crate::{HostCommand, HostError};

/// A pull request to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestDraft {
    /// Its title.
    pub title: String,
    /// Its description, in Markdown.
    pub body: String,
    /// The branch it merges into.
    pub base: String,
    /// The branch it merges from; must already be pushed.
    pub head: String,
    /// Open it as a draft.
    pub draft: bool,
}

/// An opened pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    /// Its number.
    pub number: u64,
    /// Its page.
    pub url: String,
}

/// How a pull request is merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeMethod {
    /// One commit on the base branch.
    Squash,
    /// A merge commit.
    Merge,
    /// Each commit replayed onto the base branch.
    Rebase,
}

/// The parts of a pull request's state that decide whether to merge it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestState {
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
    pub mergeable: String,
}

fn gh(directory: &Path) -> HostCommand {
    HostCommand::new("gh").cwd(directory)
}

fn create_command(directory: &Path, draft: &PullRequestDraft) -> HostCommand {
    let command =
        gh(directory).args(["pr", "create", "--title", &draft.title, "--body", &draft.body]);
    let command = command.args(["--base", &draft.base, "--head", &draft.head]);
    if draft.draft { command.arg("--draft") } else { command }
}

fn merge_command(directory: &Path, number: u64, method: MergeMethod) -> HostCommand {
    let method = match method {
        MergeMethod::Squash => "--squash",
        MergeMethod::Merge => "--merge",
        MergeMethod::Rebase => "--rebase",
    };
    gh(directory).args(["pr", "merge", &number.to_string(), method])
}

/// Opens a pull request from `directory`, a checkout of the repository.
pub fn create(
    host: &Host,
    directory: &Path,
    draft: &PullRequestDraft,
) -> Result<PullRequest, HostError> {
    let output = create_command(directory, draft).run(host)?;
    parse_created(&output).ok_or_else(|| HostError::Unexpected {
        program: "gh".into(),
        message: format!("no pull request URL in {output:?}"),
    })
}

/// Merges a pull request.
pub fn merge(
    host: &Host,
    directory: &Path,
    number: u64,
    method: MergeMethod,
) -> Result<(), HostError> {
    merge_command(directory, number, method).run(host).map(drop)
}

/// Reads whether a pull request is open and mergeable.
pub fn state(host: &Host, directory: &Path, number: u64) -> Result<PullRequestState, HostError> {
    let output = gh(directory)
        .args(["pr", "view", &number.to_string(), "--json", "state,mergeable"])
        .run(host)?;
    let value: Value = serde_json::from_str(&output).map_err(|error| HostError::Unexpected {
        program: "gh".into(),
        message: error.to_string(),
    })?;
    let field = |name: &str| value[name].as_str().unwrap_or_default().to_owned();
    Ok(PullRequestState { state: field("state"), mergeable: field("mergeable") })
}

/// `gh pr create` prints the new pull request's URL as its last line.
fn parse_created(output: &str) -> Option<PullRequest> {
    let url = output.lines().rev().map(str::trim).find(|line| line.starts_with("https://"))?;
    let number = url.trim_end_matches('/').rsplit('/').next()?.parse().ok()?;
    Some(PullRequest { number, url: url.to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_passes_every_field_and_reads_the_url_back() {
        let draft = PullRequestDraft {
            title: "Add a".into(),
            body: "Body".into(),
            base: "main".into(),
            head: "maestro/n1".into(),
            draft: true,
        };
        let command = create_command(Path::new("/w"), &draft);
        assert_eq!(
            command.args,
            [
                "pr",
                "create",
                "--title",
                "Add a",
                "--body",
                "Body",
                "--base",
                "main",
                "--head",
                "maestro/n1",
                "--draft"
            ]
        );
        let created =
            parse_created("Warning: 1 uncommitted change\nhttps://github.com/o/r/pull/12\n");
        assert_eq!(
            created,
            Some(PullRequest { number: 12, url: "https://github.com/o/r/pull/12".into() })
        );
        assert_eq!(parse_created("nothing"), None);
    }

    #[test]
    fn merge_names_the_method() {
        assert_eq!(
            merge_command(Path::new("/w"), 3, MergeMethod::Squash).args,
            ["pr", "merge", "3", "--squash"]
        );
    }
}
