//! The daemon: the one owner of Maestro's state, serving any number of frontends.
//!
//! [`State`] applies [`Command`]s and reports what changed; it is plain data and has no threads,
//! so it is tested directly. [`Hub`] puts one `State` behind a lock and serves it over any byte
//! stream: stdio when a frontend starts the daemon itself (locally or through `ssh`), or an
//! in-memory pipe in tests. Every connection gets every [`Event`], whoever caused it.

mod state;

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use maestro_protocol::{
    ClientMessage, FrameError, PROTOCOL_VERSION, ServerMessage, read_message, write_message,
};

pub use state::State;

/// Shared state plus the outgoing queue of every connection.
#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<Mutex<Inner>>,
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
            let mut inner = self.inner.lock().expect("hub lock");
            let (result, events) = inner.state.apply(command);
            outgoing.send(ServerMessage::Response { id, result }).ok();
            // Broadcast under the lock so every connection sees events in the order applied.
            for event in events {
                inner.connections.retain(|(_, connection)| {
                    connection.send(ServerMessage::Event { event: event.clone() }).is_ok()
                });
            }
        }
        Ok(())
    }
}
