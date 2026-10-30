//! The daemon: the one owner of Maestro's state, serving any number of frontends.
//!
//! [`State`] applies the workflow [`Command`]s and reports what changed; it is plain data and has
//! no threads, so it is tested directly. The runtime handles the commands with side effects
//! (agents, worktrees, pull requests), each on its own thread. [`Hub`] puts both behind one front
//! and serves it over any byte stream: stdio, a Unix socket for the resident daemon, or an
//! in-memory pipe in tests. Every connection gets every [`Event`], whoever caused it.
//!
//! [`Command`]: maestro_protocol::Command
//! [`Event`]: maestro_protocol::Event

mod engine;
pub mod resident;
mod runtime;
mod snapshot;
mod state;
mod store;

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use maestro_protocol::{
    ClientMessage, CommandError, Event, FrameError, PROTOCOL_VERSION, ServerMessage, read_message,
    write_message,
};

use engine::Job;
use runtime::Runtime;
use snapshot::Snapshot;
pub use state::State;
use store::Store;

/// The file in the daemon's home that holds its workflows and transcripts.
const STATE_FILE: &str = "state.json";

/// How often a change is written out; a crash loses at most this much.
const SAVE_EVERY: Duration = Duration::from_secs(1);

/// Shared state, the runtime, the engine, and the outgoing queue of every connection.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Mutex<Inner>>,
    runtime: Arc<Runtime>,
    engine: Sender<Job>,
}

impl Default for Hub {
    /// A hub that remembers nothing once it is dropped.
    fn default() -> Self {
        Self::with_store(Store::memory())
    }
}

pub(crate) fn broadcast(inner: &mut Inner, event: &Event) {
    // Every change reaches the frontends as an event, so every change is one to save.
    inner.changed = true;
    inner.connections.retain(|(_, connection)| {
        connection.send(ServerMessage::Event { event: event.clone() }).is_ok()
    });
}

#[derive(Default)]
pub(crate) struct Inner {
    pub(crate) state: State,
    connections: Vec<(u64, Sender<ServerMessage>)>,
    next_connection: u64,
    /// Whether anything happened since the last save.
    changed: bool,
}

impl Hub {
    /// A hub with empty state, remembering nothing once it is dropped.
    pub fn new() -> Self {
        Self::default()
    }

    /// A hub that keeps the router's catalog, its record of outcomes, and its workflows and
    /// transcripts in `home`, and takes back what was saved there before.
    pub fn open(home: &Path) -> io::Result<Self> {
        let store = Store::open(home)?;
        let path = home.join(STATE_FILE);
        let snapshot = store::read::<Snapshot>(&path)?.unwrap_or_default().settle();
        let hub = Self::with_store(store);
        let commanders: Vec<_> = snapshot
            .agents
            .iter()
            .filter(|agent| agent.spec.role == maestro_core::AgentRole::Commander)
            .map(|agent| agent.id)
            .collect();
        hub.inner.lock().expect("hub lock").state =
            State::restore(snapshot.projects, snapshot.workflows, &commanders);
        hub.runtime.restore(snapshot.agents);
        let saving = hub.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(SAVE_EVERY);
                if let Err(error) = saving.save_changes(&path) {
                    eprintln!("maestro-daemon: could not save the state: {error}");
                }
            }
        });
        Ok(hub)
    }

    /// Writes the state to `path` if anything changed since the last time.
    fn save_changes(&self, path: &Path) -> io::Result<()> {
        let (projects, workflows) = {
            let mut inner = self.inner.lock().expect("hub lock");
            if !std::mem::take(&mut inner.changed) {
                return Ok(());
            }
            (inner.state.projects(), inner.state.workflows())
        };
        let snapshot = Snapshot { projects, workflows, agents: self.runtime.saved() };
        store::write(path, &snapshot)
    }

    /// Whether nothing needs the hub: no frontend is connected and no agent is in a turn or
    /// waiting for an answer.
    pub fn idle(&self) -> bool {
        self.inner.lock().expect("hub lock").connections.is_empty() && !self.runtime.busy()
    }

    /// Stops every agent and saves the state to `home`, for a daemon about to exit.
    pub fn shut_down(&self, home: &Path) -> io::Result<()> {
        self.runtime.stop_all();
        self.inner.lock().expect("hub lock").changed = true;
        self.save_changes(&home.join(STATE_FILE))
    }

    fn with_store(store: Store) -> Self {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let shared = Arc::clone(&inner);
        let runtime = Runtime::new(Arc::new(move |event| {
            broadcast(&mut shared.lock().expect("hub lock"), &event);
        }));
        let shared = Arc::clone(&inner);
        runtime.brief_with(Box::new(move |spec| {
            let state = &shared.lock().expect("hub lock").state;
            match spec.project {
                Some(project) => state.briefing(project),
                None => maestro_core::status_report(&state.workflows()),
            }
        }));
        let engine = engine::spawn(Arc::clone(&inner), Arc::clone(&runtime), store);
        Self { inner, runtime, engine }
    }

    /// Serves one connection until its reader ends, then returns.
    ///
    /// Responses and events are written from a separate thread, so a frontend that stops reading
    /// stalls only its own connection, never the state lock.
    pub fn serve(
        &self,
        reader: impl Read,
        writer: impl Write + Send + 'static,
    ) -> Result<(), FrameError> {
        let (outgoing, queue) = mpsc::channel::<ServerMessage>();
        outgoing.send(ServerMessage::Hello { protocol: PROTOCOL_VERSION }).ok();
        let connection = {
            let mut inner = self.inner.lock().expect("hub lock");
            inner.next_connection += 1;
            let connection = inner.next_connection;
            inner.connections.push((connection, outgoing.clone()));
            connection
        };

        let writer_thread = thread::spawn(move || -> Result<(), FrameError> {
            let mut writer = writer;
            for message in queue {
                write_message(&mut writer, &message)?;
            }
            Ok(())
        });

        let result = self.read_loop(BufReader::new(reader), &outgoing);

        // Dropping this connection's senders ends its writer thread once the queue drains.
        let mut inner = self.inner.lock().expect("hub lock");
        inner.connections.retain(|(id, _)| *id != connection);
        drop(inner);
        drop(outgoing);
        let written = writer_thread.join().expect("writer thread panicked");
        result.and(written)
    }

    fn read_loop(
        &self,
        mut reader: impl BufRead,
        outgoing: &Sender<ServerMessage>,
    ) -> Result<(), FrameError> {
        while let Some(ClientMessage { id, command }) = read_message(&mut reader)? {
            if engine::handles(&command) {
                let (answer, answered) = mpsc::channel();
                self.engine.send(Job::Command(Box::new(command), answer)).ok();
                let outgoing = outgoing.clone();
                thread::spawn(move || {
                    let result = answered.recv().unwrap_or_else(|_| {
                        Err(CommandError::Host { message: "the workflow engine stopped".into() })
                    });
                    outgoing.send(ServerMessage::Response { id, result }).ok();
                });
                continue;
            }
            if Runtime::handles(&command) {
                let runtime = Arc::clone(&self.runtime);
                let outgoing = outgoing.clone();
                thread::spawn(move || {
                    let result = runtime.execute(command);
                    outgoing.send(ServerMessage::Response { id, result }).ok();
                });
                continue;
            }
            let mut inner = self.inner.lock().expect("hub lock");
            let (result, events) = inner.state.apply(command);
            outgoing.send(ServerMessage::Response { id, result }).ok();
            // Broadcast under the lock so every connection sees events in the order applied.
            for event in events {
                broadcast(&mut inner, &event);
            }
        }
        Ok(())
    }
}
