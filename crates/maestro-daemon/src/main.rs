//! The daemon's command line.
//!
//! * `maestro-daemon serve [--socket PATH]` runs the resident daemon: one [`Hub`] behind a Unix
//!   socket, outliving every frontend, so agents keep working while the laptop sleeps and a GUI
//!   and a TUI can watch the same work.
//! * `maestro-daemon --stdio [--socket PATH]` is what a frontend runs, directly or as
//!   `ssh host maestro-daemon --stdio`. It relays stdio to the resident daemon, starting one first
//!   if none answers. SSH does the authentication, so the daemon opens no port of its own.
//!
//! Diagnostics go to standard error, which `ssh` passes back separately from the protocol.

use std::path::PathBuf;
use std::process::ExitCode;

use maestro_daemon::Hub;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, socket) = match &args[..] {
        [mode] => (mode.as_str(), None),
        [mode, flag, path] if flag == "--socket" => (mode.as_str(), Some(PathBuf::from(path))),
        _ => return usage(),
    };
    let result = match mode {
        "serve" => resident::serve(socket),
        "--stdio" => resident::relay(socket),
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

fn usage() -> ExitCode {
    eprintln!("usage: maestro-daemon (serve | --stdio) [--socket PATH]");
    ExitCode::from(2)
}

#[cfg(unix)]
mod resident {
    use std::io::{self, Read, Write};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

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

    pub(super) fn serve(socket: Option<PathBuf>) -> io::Result<()> {
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
        let hub = Hub::new();
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

    pub(super) fn relay(socket: Option<PathBuf>) -> io::Result<()> {
        let socket = socket.map_or_else(default_socket, Ok)?;
        let stream = match UnixStream::connect(&socket) {
            Ok(stream) => stream,
            Err(_) => start(&socket)?,
        };
        let mut upstream = stream.try_clone()?;
        thread::spawn(move || {
            pass(&mut io::stdin().lock(), &mut upstream).ok();
            // The frontend hung up: let the daemon see the end of this connection.
            upstream.shutdown(std::net::Shutdown::Write).ok();
        });
        pass(&mut &stream, &mut io::stdout().lock())
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
mod resident {
    use std::io;
    use std::path::PathBuf;

    use super::Hub;

    pub(super) fn serve(_: Option<PathBuf>) -> io::Result<()> {
        Err(io::Error::other("the resident daemon needs a Unix host; use --stdio"))
    }

    /// Without Unix sockets the daemon lives only as long as this connection.
    pub(super) fn relay(_: Option<PathBuf>) -> io::Result<()> {
        Hub::new().serve(io::stdin(), io::stdout()).map_err(io::Error::other)
    }
}
