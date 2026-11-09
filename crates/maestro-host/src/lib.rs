//! Everything Maestro runs on a machine: agent CLIs, `git` and `gh`.
//!
//! A [`HostCommand`] describes a process the way the daemon thinks of it (program, arguments,
//! working directory, environment) and [`HostCommand::process`] turns it into a
//! [`std::process::Command`] for a [`Host`]: directly for [`Host::Local`], wrapped in `ssh` for
//! [`Host::Ssh`], in `wsl.exe` for [`Host::Wsl`] and in `docker exec` for [`Host::Docker`]. The
//! last three hand one POSIX shell line to the machine, so they share its quoting ([`shell`]);
//! nothing above this crate builds any of these command lines.
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
use std::process::{Child, Command, Output, Stdio};

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
                let mut command = Command::new(local_program(&self.program));
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
            Host::Wsl { distribution } => {
                let mut command = Command::new("wsl.exe");
                if !distribution.is_empty() {
                    command.args(["--distribution", distribution]);
                }
                // --exec skips the user's login shell, so `sh` reads the line exactly as written.
                command.args(["--exec", "sh", "-c"]).arg(self.remote_script());
                command
            }
            Host::Docker { container } => {
                let mut command = Command::new("docker");
                // -i keeps stdin open for the agent's stream; no -t, so no terminal mangles it.
                command.args(["exec", "-i", container, "sh", "-c"]).arg(self.remote_script());
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

    /// The single shell line handed to the shell on another machine.
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

/// The program to start on this machine for `program`.
///
/// On Windows, `std::process::Command` looks only for `program.exe`, so a CLI installed as a
/// `.cmd` shim (as `npm install -g` installs `codex`) would never be found; [`locate`] finds it
/// the way the Windows shell does. Elsewhere, and for a program not found, the name is returned
/// as given.
pub fn local_program(program: &str) -> PathBuf {
    #[cfg(windows)]
    if let Some(found) = locate(program) {
        return found;
    }
    PathBuf::from(program)
}

/// Where `program` is on this machine, looked up as the Windows shell does: in each directory of
/// `PATH`, with each extension in `PATHEXT`. A path is checked only where it points, and a name
/// with an extension of its own (`codex.cmd`) is also tried as written. `None` when nothing
/// matches.
#[cfg(windows)]
pub fn locate(program: &str) -> Option<PathBuf> {
    let path = std::path::Path::new(program);
    // Lower case, as installers name their files: the file system ignores case either way.
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let pathext = pathext.to_ascii_lowercase();
    let mut extensions: Vec<&str> = pathext.split(';').filter(|e| !e.is_empty()).collect();
    if path.extension().is_some() {
        extensions.insert(0, "");
    }
    let directories: Vec<PathBuf> = if path.components().count() > 1 {
        vec![PathBuf::new()]
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?).collect()
    };
    directories.iter().find_map(|directory| {
        extensions
            .iter()
            .map(|extension| directory.join(format!("{program}{extension}")))
            .find(|candidate| candidate.is_file())
    })
}

/// Kills `child` and every process it started.
///
/// On Windows a CLI installed as a `.cmd` shim runs as `cmd.exe`, which starts `node`, which may
/// start the CLI's own binary. Killing only `cmd.exe` leaves the rest running, still holding the
/// child's output open, so `taskkill /T` takes the whole tree. Elsewhere this is [`Child::kill`].
pub fn kill_tree(child: &mut Child) -> io::Result<()> {
    #[cfg(windows)]
    {
        let killed = HostCommand::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .output(&Host::Local);
        if killed.is_ok_and(|output| output.status.success()) {
            return Ok(());
        }
    }
    child.kill()
}

/// The user's home directory on this machine: `HOME`, else `USERPROFILE` (as on Windows).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
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
        // A name found nowhere, so no lookup turns it into a path.
        let command = HostCommand::new("maestro-no-such-cli")
            .args(["--print", "a b"])
            .cwd("/w")
            .env("K", "v");
        let process = command.process(&Host::Local);
        assert_eq!(process.get_program(), "maestro-no-such-cli");
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
    fn wsl_and_docker_run_the_same_line_through_sh() {
        let command = HostCommand::new("git").args(["status"]).cwd("/home/me/app").env("A", "1");
        let line = "cd /home/me/app && env A=1 git status";
        let wsl = command.process(&Host::Wsl { distribution: "Ubuntu".into() });
        assert_eq!(wsl.get_program(), "wsl.exe");
        assert_eq!(args(&wsl), ["--distribution", "Ubuntu", "--exec", "sh", "-c", line]);
        let default = command.process(&Host::Wsl { distribution: String::new() });
        assert_eq!(args(&default), ["--exec", "sh", "-c", line]);
        let docker = command.process(&Host::Docker { container: "dev".into() });
        assert_eq!(docker.get_program(), "docker");
        assert_eq!(args(&docker), ["exec", "-i", "dev", "sh", "-c", line]);
    }

    /// `npm install -g` installs a CLI as `name.cmd`, which `std::process::Command` alone never
    /// finds; one started through the shim has a tree of processes that killing has to end.
    #[cfg(windows)]
    #[test]
    fn a_cmd_shim_is_found_and_its_whole_tree_killed() {
        use std::io::Read;

        let directory = tempfile::tempdir().unwrap();
        let shim = directory.path().join("tool.cmd");
        // `ping` stands in for the `node` a real shim starts: a child that outlives `cmd.exe`.
        std::fs::write(&shim, "@echo off\r\necho started\r\nping -n 60 127.0.0.1 >nul\r\n")
            .unwrap();
        let program = directory.path().join("tool");
        assert_eq!(locate(&program.to_string_lossy()), Some(shim.clone()));
        assert_eq!(locate(&shim.to_string_lossy()), Some(shim));
        assert_eq!(locate(&directory.path().join("absent").to_string_lossy()), None);

        let mut child = HostCommand::new(program.to_string_lossy())
            .process(&Host::Local)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut first = [0; 7];
        stdout.read_exact(&mut first).unwrap();
        assert_eq!(&first, b"started");
        kill_tree(&mut child).unwrap();
        // The output ends only once `ping`, which holds it too, is gone.
        let started = std::time::Instant::now();
        stdout.read_to_end(&mut Vec::new()).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        child.wait().unwrap();
    }

    #[test]
    fn failures_carry_standard_error() {
        let error = HostCommand::new("sh").args(["-c", "echo nope >&2; exit 3"]).run(&Host::Local);
        let Err(HostError::Failed { status, stderr, .. }) = error else { panic!("{error:?}") };
        assert_eq!((status, stderr.as_str()), (Some(3), "nope"));
    }
}
