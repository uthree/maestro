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

mod runtime;
mod state;

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use maestro_protocol::{
    ClientMessage, Event, FrameError, PROTOCOL_VERSION, ServerMessage, read_message, write_message,
};

use runtime::Runtime;
pub use state::State;

/// Shared state, the runtime, and the outgoing queue of every connection.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Mutex<Inner>>,
    runtime: Arc<Runtime>,
}

impl Default for Hub {
    fn default() -> Self {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let shared = Arc::clone(&inner);
        let runtime = Runtime::new(Arc::new(move |event| {
            broadcast(&mut shared.lock().expect("hub lock"), &event);
        }));
        Self { inner, runtime }
    }
}

fn broadcast(inner: &mut Inner, event: &Event) {
    inner.connections.retain(|(_, connection)| {
        connection.send(ServerMessage::Event { event: event.clone() }).is_ok()
    });
}

#[derive(Default)]
struct Inner {
    state: State,
    connections: Vec<(u64, Sender<ServerMessage>)>,
    next_connection: u64,
}

impl Hub {
    /// A hub with empty state.
    pub fn new() -> Self {
        Self::default()
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
