//! The connection to the daemon: opening it, and turning its events into a stream the window's
//! tasks can await.
//!
//! Everything here blocks on the network, so it runs on gpui's background executor or on its
//! own thread, never on the thread that draws.

use std::sync::Arc;
use std::thread;

use futures::channel::mpsc::{self, UnboundedReceiver};
use maestro_client::{Client, ClientError, Launch, Mirror};
use maestro_protocol::{Event, PROTOCOL_VERSION};

use crate::i18n::tr;
use crate::trf;

/// A connected daemon, with everything it had when the window connected.
pub struct Opened {
    /// The connection; requests may be made from any thread.
    pub client: Arc<Client>,
    /// The daemon's state as of the connection.
    pub mirror: Mirror,
    /// Every event after that.
    pub events: UnboundedReceiver<Event>,
}

/// Connects and loads the daemon's state. Blocks for several round trips.
pub fn open(launch: &Launch) -> Result<Opened, ClientError> {
    let client = Arc::new(Client::connect(launch)?);
    let incoming = client.take_events().ok_or(ClientError::Disconnected)?;
    // What happened before the load is in the load; only later events move the copy on.
    while incoming.try_recv().is_ok() {}
    let mirror = Mirror::load(&client)?;
    let (sender, events) = mpsc::unbounded();
    thread::Builder::new()
        .name("maestro-events".into())
        .spawn(move || {
            while let Ok(event) = incoming.recv() {
                if sender.unbounded_send(event).is_err() {
                    break;
                }
            }
        })
        .map_err(ClientError::Spawn)?;
    Ok(Opened { client, mirror, events })
}

/// Where the daemon is, in words for the window's chrome.
pub fn describe(launch: &Launch) -> String {
    match launch {
        Launch::InProcess(_) => tr("this window", "このウィンドウ").into(),
        Launch::Local { .. } => tr("this machine", "このマシン").into(),
        Launch::Ssh { destination, .. } => destination.clone(),
    }
}

/// Why the connection failed, and what to do about it.
pub fn explain(error: &ClientError, launch: &Launch) -> String {
    let place = describe(launch);
    match error {
        ClientError::Version { daemon } => trf!(
            "The maestro-daemon on {place} speaks protocol {daemon}, and this app speaks \
             {PROTOCOL_VERSION}. Install the maestro-daemon built with this app there \
             (cargo build --workspace), then try again.",
            "{place}の maestro-daemon はプロトコル {daemon} で、このアプリは {PROTOCOL_VERSION} \
             です。このアプリと同じビルドの maestro-daemon をそこにインストールしてから \
             (cargo build --workspace)、再試行してください。"
        ),
        error => {
            trf!(
                "{error}. Check that maestro-daemon is installed on {place} and try again.",
                "{error}。{place}に maestro-daemon がインストールされているか確認して、再試行してください。"
            )
        }
    }
}
