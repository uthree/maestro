//! A frontend's handle on a daemon, wherever the daemon runs.
//!
//! The GUI and the TUI both hold a [`Client`] and call the same [`Client::request`], so a feature
//! added to the protocol is available to both at once. How the bytes travel is chosen once, by
//! [`Launch`]:
//!
//! * [`Launch::InProcess`] runs a [`Hub`] on a thread of this process — for tests, and for a
//!   frontend used on the same machine that does the work.
//! * [`Launch::Local`] starts `maestro-daemon --stdio` as a child process.
//! * [`Launch::Ssh`] starts it on another machine through the system `ssh`, so the user's own
//!   config, keys and agent apply and Maestro never handles a credential.

use std::collections::HashMap;
use std::io::{self, BufReader, Read, Write};
use std::process::{Child, Command as Process, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use maestro_core::Host;
use maestro_daemon::Hub;
use maestro_host::HostCommand;
use maestro_protocol::{
    ClientMessage, Command, CommandError, Event, FrameError, PROTOCOL_VERSION, Reply, RequestId,
    ServerMessage, read_message, write_message,
};
use thiserror::Error;

/// The daemon executable's name when none is given.
pub const DAEMON: &str = "maestro-daemon";

/// Where to find, or how to start, the daemon.
#[derive(Clone)]
pub enum Launch {
    /// A hub running on a thread of this process.
    InProcess(Hub),
    /// `program --stdio` on this machine.
    Local {
        /// Path or name of the daemon executable.
        program: String,
    },
    /// `program --stdio` on another machine, through `ssh`.
    Ssh {
        /// Anything `ssh` accepts as a destination.
        destination: String,
        /// Path or name of the daemon executable on that machine.
        program: String,
    },
}

impl Launch {
    /// The daemon on another machine, found on its `PATH`.
    pub fn ssh(destination: impl Into<String>) -> Self {
        Self::Ssh { destination: destination.into(), program: DAEMON.into() }
    }

    /// The process this launch starts, if it starts one.
    fn process(&self) -> Option<Process> {
        let (host, program) = match self {
            Self::InProcess(_) => return None,
            Self::Local { program } => (Host::Local, program),
            Self::Ssh { destination, program } => {
                (Host::Ssh { destination: destination.clone() }, program)
            }
        };
        Some(HostCommand::new(program).arg("--stdio").process(&host))
    }
}

/// Why connecting or a request failed.
#[derive(Debug, Error)]
pub enum ClientError {
    /// The daemon could not be started.
    #[error("could not start the daemon: {0}")]
    Spawn(#[source] io::Error),
    /// The stream broke or carried something unreadable.
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// The daemon speaks another version of the protocol.
    #[error("the daemon speaks protocol {daemon}, this frontend speaks {PROTOCOL_VERSION}")]
    Version {
        /// The daemon's version.
        daemon: u32,
    },
    /// The daemon went away, or never said hello.
    #[error("the daemon disconnected")]
    Disconnected,
    /// The daemon refused the command.
    #[error(transparent)]
    Refused(#[from] CommandError),
}

type Pending = Arc<Mutex<HashMap<RequestId, Sender<Result<Reply, CommandError>>>>>;

/// A live connection to a daemon. Requests may be made from any thread.
pub struct Client {
    writer: Mutex<Box<dyn Write + Send>>,
    pending: Pending,
    next_id: AtomicU64,
    events: Mutex<Option<Receiver<Event>>>,
    child: Option<Child>,
}

impl Client {
    /// Connects, and waits for the daemon's hello before returning.
    pub fn connect(launch: &Launch) -> Result<Self, ClientError> {
        let (reader, writer, child): (Box<dyn Read + Send>, Box<dyn Write + Send>, _) =
            match launch.process() {
                None => {
                    let Launch::InProcess(hub) = launch else { unreachable!() };
                    let (theirs_in, ours_out) = io::pipe().map_err(ClientError::Spawn)?;
                    let (ours_in, theirs_out) = io::pipe().map_err(ClientError::Spawn)?;
                    let hub = hub.clone();
                    thread::spawn(move || hub.serve(theirs_in, theirs_out));
                    (Box::new(ours_in), Box::new(ours_out), None)
                }
                Some(mut process) => {
                    let mut child = process
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::inherit())
                        .spawn()
                        .map_err(ClientError::Spawn)?;
                    let stdin = child.stdin.take().expect("stdin was piped");
                    let stdout = child.stdout.take().expect("stdout was piped");
                    (Box::new(stdout), Box::new(stdin), Some(child))
                }
            };

        let mut reader = BufReader::new(reader);
        match read_message::<ServerMessage>(&mut reader)? {
            Some(ServerMessage::Hello { protocol }) if protocol == PROTOCOL_VERSION => {}
            Some(ServerMessage::Hello { protocol }) => {
                return Err(ClientError::Version { daemon: protocol });
            }
            _ => return Err(ClientError::Disconnected),
        }

        let pending: Pending = Arc::default();
        let (events_in, events_out) = mpsc::channel();
        let routes = Arc::clone(&pending);
        thread::spawn(move || {
            loop {
                let message = match read_message::<ServerMessage>(&mut reader) {
                    Ok(Some(message)) => message,
                    Ok(None) => break,
                    Err(error) => {
                        eprintln!("maestro-client: {error}");
                        break;
                    }
                };
                match message {
                    ServerMessage::Response { id, result } => {
                        if let Some(waiter) = routes.lock().expect("pending lock").remove(&id) {
                            waiter.send(result).ok();
                        }
                    }
                    ServerMessage::Event { event } => {
                        events_in.send(event).ok();
                    }
                    ServerMessage::Hello { .. } => {}
                }
            }
            // Wake every waiter: their senders drop here and `request` sees a disconnect.
            routes.lock().expect("pending lock").clear();
        });

        Ok(Self {
            writer: Mutex::new(writer),
            pending,
            next_id: AtomicU64::new(1),
            events: Mutex::new(Some(events_out)),
            child,
        })
    }

    /// Sends a command and blocks until the daemon answers it.
    ///
    /// A frontend calls this from a background task, never from its UI thread: over SSH an
    /// answer is a network round trip away.
    pub fn request(&self, command: Command) -> Result<Reply, ClientError> {
        let id = RequestId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (answer_in, answer_out) = mpsc::channel();
        self.pending.lock().expect("pending lock").insert(id, answer_in);
        let sent = write_message(
            &mut *self.writer.lock().expect("writer lock"),
            &ClientMessage { id, command },
        );
        if let Err(error) = sent {
            self.pending.lock().expect("pending lock").remove(&id);
            return Err(error.into());
        }
        Ok(answer_out.recv().map_err(|_| ClientError::Disconnected)??)
    }

    /// The stream of broadcast events. It can be taken once; later calls return `None`.
    pub fn take_events(&self) -> Option<Receiver<Event>> {
        self.events.lock().expect("events lock").take()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            // Closing stdin ends the daemon's read loop; kill covers a daemon that hangs.
            *self.writer.get_mut().expect("writer lock") = Box::new(io::sink());
            if !matches!(child.try_wait(), Ok(Some(_))) {
                child.kill().ok();
            }
            child.wait().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_launch_runs_the_daemon_without_a_terminal_or_prompts() {
        let process = Launch::ssh("build-box").process().unwrap();
        let args: Vec<_> = process.get_args().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(process.get_program(), "ssh");
        assert_eq!(
            args,
            ["-T", "-o", "BatchMode=yes", "build-box", "--", "maestro-daemon --stdio"]
        );
    }
}
