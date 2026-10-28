//! Everything Maestro runs on a machine: agent CLIs, `git` and `gh`.
//!
//! A [`HostCommand`] describes a process the way the daemon thinks of it (program, arguments,
//! working directory, environment) and [`HostCommand::process`] turns it into a
//! [`std::process::Command`] for a [`Host`]: directly for [`Host::Local`], or wrapped in `ssh` for
//! [`Host::Ssh`]. Nothing above this crate builds an `ssh` command line, so quoting lives in one
//! place ([`shell`]).
//!
//! [`git`] manages the per-worker worktrees and the commits and pushes made in them; [`github`]
//! opens and merges pull requests through `gh`, signed in on that host, so the daemon never holds
//! a GitHub token.

pub mod git;
pub mod github;
pub mod shell;

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use maestro_core::Host;
use thiserror::Error;

/// A process to start on some host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostCommand {
    /// The executable, looked up on the host's `PATH` unless it is a path.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// The directory it starts in, on the host.
    pub cwd: Option<PathBuf>,
    /// Variables to set.
    pub env: BTreeMap<String, String>,
    /// Variables to clear, so a worker never inherits another agent's runtime.
    pub env_remove: Vec<String>,
}

impl HostCommand {
    /// A command with no arguments.
    pub fn new(program: impl Into<String>) -> Self {
        Self { program: program.into(), ..Self::default() }
    }

    /// Appends one argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Appends several arguments.
    pub fn args<I: IntoIterator<Item = S>, S: Into<String>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets the working directory.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Sets an environment variable.
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }

    /// Clears an environment variable.
    pub fn env_remove(mut self, name: impl Into<String>) -> Self {
        self.env_remove.push(name.into());
        self
    }

    /// The process that runs this command on `host`. Standard streams are left to the caller.
    pub fn process(&self, host: &Host) -> Command {
        match host {
            Host::Local => {
                let mut command = Command::new(&self.program);
                command.args(&self.args);
                if let Some(cwd) = &self.cwd {
                    command.current_dir(cwd);
                }
                for name in &self.env_remove {
                    command.env_remove(name);
                }
                command.envs(&self.env);
                command
            }
            Host::Ssh { destination } => {
                let mut command = Command::new("ssh");
                // -T: no terminal, so the stream carries only the program's own bytes.
                // BatchMode: fail instead of prompting on a stdin that belongs to the program.
                command.args(["-T", "-o", "BatchMode=yes", destination, "--"]);
                command.arg(self.remote_script());
                command
            }
        }
    }

    /// Runs to completion with stdin closed and captures its output.
    pub fn output(&self, host: &Host) -> Result<Output, HostError> {
        self.process(host)
            .stdin(Stdio::null())
            .output()
            .map_err(|source| HostError::Spawn { program: self.program.clone(), source })
    }

    /// Runs to completion and returns standard output, or an error carrying standard error.
    pub fn run(&self, host: &Host) -> Result<String, HostError> {
        let output = self.output(host)?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(HostError::Failed {
                program: self.program.clone(),
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }

    /// The single shell line `ssh` hands to the remote shell.
    fn remote_script(&self) -> String {
        let mut words = Vec::new();
        if let Some(cwd) = &self.cwd {
            words.push(format!("cd {} &&", shell::quote(&cwd.to_string_lossy())));
        }
        // `env` sets and clears variables without depending on the remote shell's syntax.
        if !self.env.is_empty() || !self.env_remove.is_empty() {
            words.push("env".into());
            for name in &self.env_remove {
                words.push(format!("-u {}", shell::quote(name)));
            }
            for (name, value) in &self.env {
                words.push(shell::quote(&format!("{name}={value}")));
            }
        }
        words.push(shell::quote(&self.program));
        words.extend(self.args.iter().map(|arg| shell::quote(arg)));
        words.join(" ")
    }
}

/// A command could not be run, or ran and failed.
#[derive(Debug, Error)]
pub enum HostError {
    /// The process could not be started at all.
    #[error("could not start {program}: {source}")]
    Spawn {
        /// The program.
        program: String,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// The process exited unsuccessfully.
    #[error("{program} failed ({}): {stderr}", status.map_or("killed".into(), |code| format!("exit {code}")))]
    Failed {
        /// The program.
        program: String,
        /// Its exit code, when it exited rather than being killed.
        status: Option<i32>,
        /// What it wrote to standard error.
        stderr: String,
    },
    /// The process succeeded but printed something unexpected.
    #[error("unexpected output from {program}: {message}")]
    Unexpected {
        /// The program.
        program: String,
        /// What was wrong.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(command: &Command) -> Vec<String> {
        command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_local_command_runs_as_given() {
        let command = HostCommand::new("claude").args(["--print", "a b"]).cwd("/w").env("K", "v");
        let process = command.process(&Host::Local);
        assert_eq!(process.get_program(), "claude");
        assert_eq!(args(&process), ["--print", "a b"]);
        assert_eq!(process.get_current_dir(), Some(std::path::Path::new("/w")));
    }

    #[test]
    fn an_ssh_command_carries_directory_environment_and_quoting_in_one_line() {
        let command = HostCommand::new("claude")
            .args(["--model", "it's"])
            .cwd("/srv/my repo")
            .env("A", "1")
            .env_remove("CLAUDECODE");
        let process = command.process(&Host::Ssh { destination: "box".into() });
        assert_eq!(process.get_program(), "ssh");
        assert_eq!(
            args(&process),
            [
                "-T",
                "-o",
                "BatchMode=yes",
                "box",
                "--",
                r"cd '/srv/my repo' && env -u CLAUDECODE A=1 claude --model 'it'\''s'",
            ]
        );
    }

    #[test]
    fn failures_carry_standard_error() {
        let error = HostCommand::new("sh").args(["-c", "echo nope >&2; exit 3"]).run(&Host::Local);
        let Err(HostError::Failed { status, stderr, .. }) = error else { panic!("{error:?}") };
        assert_eq!((status, stderr.as_str()), (Some(3), "nope"));
    }
}
