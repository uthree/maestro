//! The daemon's command line, shared by `maestro-daemon` and any frontend that carries the
//! daemon in its own executable.
//!
//! * `maestro-daemon serve [--socket PATH] [--idle SECONDS]` runs the resident daemon: one
//!   [`Hub`] behind a Unix socket, outliving every frontend, so agents keep working while the
//!   laptop sleeps and a GUI and a TUI can watch the same work. It keeps the router's catalog
//!   (`router.json`, the user's to edit), its record of outcomes, and its workflows and
//!   transcripts (`state.json`) in `$MAESTRO_HOME`, or `~/.maestro`. Once no frontend has been
//!   connected and no agent has been busy for `--idle` seconds (ten minutes unless given; `0`
//!   for never), it saves, stops its agents and exits; the next frontend starts it again.
//! * `maestro-daemon --stdio [--socket PATH]` is what a frontend runs, directly or as
//!   `ssh host maestro-daemon --stdio`. It relays stdio to the resident daemon, starting one first
//!   if none answers, and replacing one that speaks an older protocol than its own (left running
//!   from before an upgrade). SSH does the authentication, so the daemon opens no port of its own.
//!
//! Diagnostics go to standard error, which `ssh` passes back separately from the protocol.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::Hub;

/// Whether `args` (without the program's name) ask for the daemon rather than a frontend, so a
/// frontend's executable can carry the daemon too.
pub fn requested(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("serve" | "--stdio"))
}

/// Runs the daemon's command line. `args` leave out the program's name; the resident daemon is
/// started as this same executable, so a frontend that calls this when [`requested`] is its own
/// daemon.
pub fn run(args: &[String]) -> ExitCode {
    let Some((mode, options)) = args.split_first() else { return usage() };
    let mut socket = None;
    let mut idle = Some(DEFAULT_IDLE);
    for pair in options.chunks(2) {
        match pair {
            [flag, path] if flag == "--socket" => socket = Some(PathBuf::from(path)),
            [flag, seconds] if flag == "--idle" && mode == "serve" => match seconds.parse() {
                Ok(0) => idle = None,
                Ok(seconds) => idle = Some(Duration::from_secs(seconds)),
                Err(_) => return usage(),
            },
            _ => return usage(),
        }
    }
    let result = match mode.as_str() {
        "serve" => platform::serve(socket, idle),
        "--stdio" => platform::relay(socket),
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("maestro-daemon: {error}");
            ExitCode::FAILURE
        }
    }
}

/// How long the resident daemon waits with nothing to do before it exits.
const DEFAULT_IDLE: Duration = Duration::from_secs(10 * 60);

fn usage() -> ExitCode {
    eprintln!("usage: maestro-daemon (serve [--idle SECONDS] | --stdio) [--socket PATH]");
    ExitCode::from(2)
}

#[cfg(unix)]
mod platform {
    use std::io::{self, Read, Write};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use maestro_protocol::{PROTOCOL_VERSION, ServerMessage};

    use super::Hub;

    /// `$XDG_RUNTIME_DIR/maestro/daemon.sock`, or `~/.maestro/daemon.sock` without one.
    fn default_socket() -> io::Result<PathBuf> {
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
            return Ok(PathBuf::from(runtime).join("maestro/daemon.sock"));
        }
        let home = std::env::var_os("HOME")
            .ok_or_else(|| io::Error::other("neither XDG_RUNTIME_DIR nor HOME is set"))?;
        Ok(PathBuf::from(home).join(".maestro/daemon.sock"))
    }

    /// Where the daemon keeps what it remembers: `$MAESTRO_HOME`, or `~/.maestro`.
    fn home() -> io::Result<PathBuf> {
        if let Some(home) = std::env::var_os("MAESTRO_HOME") {
            return Ok(home.into());
        }
        let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
        Ok(PathBuf::from(home).join(".maestro"))
    }

    pub(super) fn serve(socket: Option<PathBuf>, idle: Option<Duration>) -> io::Result<()> {
        let socket = socket.map_or_else(default_socket, Ok)?;
        if let Some(directory) = socket.parent() {
            // Only this user may reach the socket: it can start processes as them.
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(directory)?;
        }
        if UnixStream::connect(&socket).is_ok() {
            return Err(io::Error::other(format!("a daemon already serves {}", socket.display())));
        }
        // Nothing answers, so a socket file left here is stale.
        match std::fs::remove_file(&socket) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let listener = UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        // Beside the socket, so `kill $(cat daemon.pid)` stops a daemon nothing else can reach.
        std::fs::write(socket.with_extension("pid"), std::process::id().to_string())?;
        let home = home()?;
        let hub = Hub::open(&home)?;
        if let Some(limit) = idle {
            let hub = hub.clone();
            thread::spawn(move || exit_when_idle(&hub, limit, &socket, &home));
        }
        for stream in listener.incoming() {
            let stream = stream?;
            let hub = hub.clone();
            thread::spawn(move || {
                let reader = stream.try_clone()?;
                if let Err(error) = hub.serve(reader, stream) {
                    eprintln!("maestro-daemon: connection ended: {error}");
                }
                io::Result::Ok(())
            });
        }
        Ok(())
    }

    /// Exits the process once the hub has been idle for `limit`.
    fn exit_when_idle(hub: &Hub, limit: Duration, socket: &Path, home: &Path) {
        let mut since = None;
        loop {
            thread::sleep(Duration::from_secs(1).min(limit));
            if !hub.idle() {
                since = None;
                continue;
            }
            if since.get_or_insert_with(Instant::now).elapsed() < limit {
                continue;
            }
            // Stop answering first, so a frontend arriving now starts a new daemon.
            std::fs::remove_file(socket).ok();
            std::fs::remove_file(socket.with_extension("pid")).ok();
            if let Err(error) = hub.shut_down(home) {
                eprintln!("maestro-daemon: could not save the state: {error}");
            }
            std::process::exit(0);
        }
    }

    pub(super) fn relay(socket: Option<PathBuf>) -> io::Result<()> {
        let socket = socket.map_or_else(default_socket, Ok)?;
        let mut stream = match UnixStream::connect(&socket) {
            Ok(stream) => stream,
            Err(_) => start(&socket)?,
        };
        let mut hello = match greeting(&mut stream) {
            Ok(hello) => hello,
            // It was exiting for want of work just as this connected.
            Err(_) => {
                stream = start(&socket)?;
                greeting(&mut stream)?
            }
        };
        if let Ok(ServerMessage::Hello { protocol }) = serde_json::from_slice(&hello)
            && protocol < PROTOCOL_VERSION
        {
            // This build would start a daemon that speaks this frontend's protocol; the old one
            // can only refuse it. A newer daemon is left alone for its own frontends.
            eprintln!(
                "maestro-daemon: replacing the resident daemon, which speaks protocol {protocol}; \
                 agents it was running are no longer watched"
            );
            stop(&socket)?;
            stream = start(&socket)?;
            hello = greeting(&mut stream)?;
        }
        {
            let mut stdout = io::stdout().lock();
            stdout.write_all(&hello)?;
            stdout.flush()?;
        }
        let mut upstream = stream.try_clone()?;
        thread::spawn(move || {
            pass(&mut io::stdin().lock(), &mut upstream).ok();
            // The frontend hung up: let the daemon see the end of this connection.
            upstream.shutdown(std::net::Shutdown::Write).ok();
        });
        pass(&mut &stream, &mut io::stdout().lock())
    }

    /// The daemon's first line, read a byte at a time so nothing after it is taken from the
    /// stream.
    fn greeting(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        let mut byte = [0];
        while line.last() != Some(&b'\n') {
            if stream.read(&mut byte)? == 0 {
                return Err(io::Error::other("the resident daemon closed before greeting"));
            }
            line.push(byte[0]);
        }
        Ok(line)
    }

    /// Stops the daemon whose pid file is beside `socket`, and waits until it stops answering.
    fn stop(socket: &Path) -> io::Result<()> {
        let pid = std::fs::read_to_string(socket.with_extension("pid"))?;
        let killed = Command::new("kill").arg(pid.trim()).stderr(Stdio::null()).status()?;
        if !killed.success() {
            return Err(io::Error::other(format!("could not stop daemon {}", pid.trim())));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(socket).is_ok() {
            if Instant::now() > deadline {
                return Err(io::Error::other("the old daemon is still answering"));
            }
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// Copies until the end of `from`, flushing after every read so a message is never held back.
    /// (`io::copy` would splice between the socket and a pipe, which can wait for a full page.)
    fn pass(from: &mut impl Read, to: &mut impl Write) -> io::Result<()> {
        let mut buffer = [0; 16 * 1024];
        loop {
            let read = match from.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            to.write_all(&buffer[..read])?;
            to.flush()?;
        }
    }

    /// Starts `serve` in its own session, detached from this relay and from `ssh`, and waits for
    /// it to answer.
    fn start(socket: &PathBuf) -> io::Result<UnixStream> {
        Command::new(std::env::current_exe()?)
            .args(["serve", "--socket"])
            .arg(socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match UnixStream::connect(socket) {
                Ok(stream) => return Ok(stream),
                Err(error) if Instant::now() > deadline => return Err(error),
                Err(_) => thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use std::io;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::Hub;

    pub(super) fn serve(_: Option<PathBuf>, _: Option<Duration>) -> io::Result<()> {
        Err(io::Error::other("the resident daemon needs a Unix host; use --stdio"))
    }

    /// Without Unix sockets the daemon lives only as long as this connection.
    pub(super) fn relay(_: Option<PathBuf>) -> io::Result<()> {
        Hub::new().serve(io::stdin(), io::stdout()).map_err(io::Error::other)
    }
}
