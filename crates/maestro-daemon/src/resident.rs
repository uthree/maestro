//! The daemon's command line, shared by `maestro-daemon` and any frontend that carries the
//! daemon in its own executable.
//!
//! * `maestro-daemon serve [--socket PATH] [--idle SECONDS]` runs the resident daemon: one
//!   [`Hub`] behind a Unix socket (on Windows, a loopback port), outliving every frontend, so agents keep working while the
//!   laptop sleeps and a GUI and a TUI can watch the same work. It keeps the router's catalog
//!   (`router.json`, the user's to edit), its record of outcomes, and its workflows and
//!   transcripts (`state.json`) in `$MAESTRO_HOME`, or `~/.maestro`. Once no frontend has been
//!   connected and no agent has been busy for `--idle` seconds (ten minutes unless given; `0`
//!   for never), it saves, stops its agents and exits; the next frontend starts it again.
//! * `maestro-daemon --stdio [--socket PATH]` is what a frontend runs, directly or as
//!   `ssh host maestro-daemon --stdio`. It relays stdio to the resident daemon, starting one first
//!   if none answers, and replacing one that speaks an older protocol than its own (left running
//!   from before an upgrade). SSH does the authentication, so the daemon opens no port of its own.
//!   On Windows the home is `%USERPROFILE%\.maestro` when `HOME` is unset, and `--socket` names
//!   the file that holds the daemon's port (`daemon.address` in the home unless given).
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

/// Where the daemon keeps what it remembers: `$MAESTRO_HOME`, or `.maestro` in the user's home.
fn home() -> std::io::Result<PathBuf> {
    if let Some(home) = std::env::var_os("MAESTRO_HOME") {
        return Ok(home.into());
    }
    let home = maestro_host::home_dir()
        .ok_or_else(|| std::io::Error::other("neither HOME nor USERPROFILE is set"))?;
    Ok(home.join(".maestro"))
}

/// The daemon's first line, read a byte at a time so nothing after it is taken from the stream.
#[cfg(any(unix, windows))]
fn greeting(stream: &mut impl std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0];
    while line.last() != Some(&b'\n') {
        if stream.read(&mut byte)? == 0 {
            return Err(std::io::Error::other("the resident daemon closed before greeting"));
        }
        line.push(byte[0]);
    }
    Ok(line)
}

/// Copies until the end of `from`, flushing after every read so a message is never held back.
/// (`io::copy` would splice between the socket and a pipe, which can wait for a full page.)
#[cfg(any(unix, windows))]
fn pass(from: &mut impl std::io::Read, to: &mut impl std::io::Write) -> std::io::Result<()> {
    let mut buffer = [0; 16 * 1024];
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        to.write_all(&buffer[..read])?;
        to.flush()?;
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: maestro-daemon (serve [--idle SECONDS] | --stdio) [--socket PATH]");
    ExitCode::from(2)
}

#[cfg(unix)]
mod platform {
    use std::io::{self, Write};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use maestro_protocol::{PROTOCOL_VERSION, ServerMessage};

    use super::{Hub, greeting, home, pass};

    /// `$XDG_RUNTIME_DIR/maestro/daemon.sock`, or `~/.maestro/daemon.sock` without one.
    fn default_socket() -> io::Result<PathBuf> {
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
            return Ok(PathBuf::from(runtime).join("maestro/daemon.sock"));
        }
        let home = std::env::var_os("HOME")
            .ok_or_else(|| io::Error::other("neither XDG_RUNTIME_DIR nor HOME is set"))?;
        Ok(PathBuf::from(home).join(".maestro/daemon.sock"))
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

/// Windows has no Unix sockets in `std`, so the daemon listens on a loopback TCP port instead.
/// Any local user can reach such a port, so the address file (`--socket`, by default
/// `daemon.address` in the daemon's home, which only its user can read) holds a random token
/// beside the port, and a connection that does not send it first is closed.
#[cfg(windows)]
mod platform {
    use std::fs::OpenOptions;
    use std::hash::{BuildHasher, RandomState};
    use std::io::{self, Read, Write};
    use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime};

    use maestro_protocol::{PROTOCOL_VERSION, ServerMessage};

    use super::{Hub, greeting, home, pass};

    /// `CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW`: no console window, and no Ctrl+C from the
    /// terminal that started the frontend.
    const DETACHED: u32 = 0x0000_0200 | 0x0800_0000;
    /// `CREATE_BREAKAWAY_FROM_JOB`: out of a job the frontend runs in, which may kill everything
    /// in it when the frontend closes. A job that forbids it refuses the start.
    const BREAKAWAY: u32 = 0x0100_0000;
    /// How long a live daemon on this machine takes to accept, with room to spare.
    const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

    fn default_address() -> io::Result<PathBuf> {
        Ok(home()?.join("daemon.address"))
    }

    pub(super) fn serve(address: Option<PathBuf>, idle: Option<Duration>) -> io::Result<()> {
        let address = address.map_or_else(default_address, Ok)?;
        if let Some(directory) = address.parent() {
            std::fs::create_dir_all(directory)?;
        }
        // One daemon per address: held until this process ends, however it ends, so two
        // frontends starting at once cannot start two daemons.
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(address.with_extension("lock"))?;
        if lock.try_lock().is_err() {
            return Err(io::Error::other(format!("a daemon already serves {}", address.display())));
        }
        let home = home()?;
        let hub = Hub::open(&home)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let token = token();
        std::fs::write(address.with_extension("pid"), std::process::id().to_string())?;
        // Written whole and then moved into place, so a relay never reads half of it.
        let partial = address.with_extension("partial");
        std::fs::write(&partial, format!("{} {token}\n", listener.local_addr()?.port()))?;
        std::fs::rename(&partial, &address)?;
        if let Some(limit) = idle {
            let hub = hub.clone();
            thread::spawn(move || exit_when_idle(&hub, limit, &address, &home));
        }
        for stream in listener.incoming() {
            let stream = stream?;
            let hub = hub.clone();
            let token = token.clone();
            thread::spawn(move || {
                if !admitted(&stream, &token) {
                    return io::Result::Ok(());
                }
                let reader = stream.try_clone()?;
                if let Err(error) = hub.serve(reader, stream) {
                    eprintln!("maestro-daemon: connection ended: {error}");
                }
                Ok(())
            });
        }
        drop(lock);
        Ok(())
    }

    /// 128 bits from the keys `std` seeds from the system's random source.
    fn token() -> String {
        (0..2u8)
            .map(|part| {
                let hash =
                    RandomState::new().hash_one((part, std::process::id(), SystemTime::now()));
                format!("{hash:016x}")
            })
            .collect()
    }

    /// Whether the connection's first line is `token`.
    fn admitted(mut stream: &TcpStream, token: &str) -> bool {
        // Long enough for any relay, short enough that a silent caller holds no thread.
        if stream.set_read_timeout(Some(Duration::from_secs(5))).is_err() {
            return false;
        }
        let mut line = Vec::new();
        let mut byte = [0];
        while line.len() <= token.len() {
            match stream.read(&mut byte) {
                Ok(1) if byte[0] == b'\n' => {
                    return line == token.as_bytes() && stream.set_read_timeout(None).is_ok();
                }
                Ok(1) => line.push(byte[0]),
                _ => return false,
            }
        }
        false
    }

    /// Exits the process once the hub has been idle for `limit`.
    fn exit_when_idle(hub: &Hub, limit: Duration, address: &Path, home: &Path) {
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
            std::fs::remove_file(address).ok();
            std::fs::remove_file(address.with_extension("pid")).ok();
            if let Err(error) = hub.shut_down(home) {
                eprintln!("maestro-daemon: could not save the state: {error}");
            }
            std::process::exit(0);
        }
    }

    /// Connects to the daemon the address file names and gives it the token.
    fn connect(address: &Path) -> io::Result<TcpStream> {
        let text = std::fs::read_to_string(address)?;
        let (port, token) = text
            .trim()
            .split_once(' ')
            .ok_or_else(|| io::Error::other(format!("{} is unreadable", address.display())))?;
        let port: u16 = port.parse().map_err(io::Error::other)?;
        let mut stream = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            CONNECT_TIMEOUT,
        )?;
        stream.write_all(format!("{token}\n").as_bytes())?;
        Ok(stream)
    }

    pub(super) fn relay(address: Option<PathBuf>) -> io::Result<()> {
        let address = address.map_or_else(default_address, Ok)?;
        let mut stream = match connect(&address) {
            Ok(stream) => stream,
            Err(_) => start(&address)?,
        };
        let mut hello = match greeting(&mut stream) {
            Ok(hello) => hello,
            // It was exiting for want of work just as this connected.
            Err(_) => {
                stream = start(&address)?;
                greeting(&mut stream)?
            }
        };
        if let Ok(ServerMessage::Hello { protocol }) = serde_json::from_slice(&hello)
            && protocol < PROTOCOL_VERSION
        {
            // As on Unix: an older daemon can only refuse this frontend; a newer one is left be.
            eprintln!(
                "maestro-daemon: replacing the resident daemon, which speaks protocol {protocol}"
            );
            stop(&address)?;
            stream = start(&address)?;
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
            upstream.shutdown(Shutdown::Write).ok();
        });
        pass(&mut &stream, &mut io::stdout().lock())
    }

    /// Stops the daemon whose pid file is beside `address`, with the agents it started, and
    /// waits until it stops answering.
    fn stop(address: &Path) -> io::Result<()> {
        let pid = std::fs::read_to_string(address.with_extension("pid"))?;
        let killed = Command::new("taskkill")
            .args(["/T", "/F", "/PID", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !killed.success() {
            return Err(io::Error::other(format!("could not stop daemon {}", pid.trim())));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while connect(address).is_ok() {
            if Instant::now() > deadline {
                return Err(io::Error::other("the old daemon is still answering"));
            }
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// Stops a process started from here inheriting this one's standard streams.
    ///
    /// Windows hands a new process every inheritable handle of its parent, whatever its own
    /// stdio is set to. The relay's streams are the frontend's pipes, so a daemon holding them
    /// would keep the frontend from ever seeing the relay's output end.
    fn keep_stdio_private() {
        use std::ffi::c_void;
        use std::os::windows::io::AsRawHandle;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
        }
        const HANDLE_FLAG_INHERIT: u32 = 1;
        let handles = [
            io::stdin().as_raw_handle(),
            io::stdout().as_raw_handle(),
            io::stderr().as_raw_handle(),
        ];
        for handle in handles.into_iter().filter(|handle| !handle.is_null()) {
            // SAFETY: the handle is this process's own standard stream, open for its lifetime;
            // only its inherit flag changes. A failure leaves it as it was.
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
    }

    /// Starts `serve` detached from this relay, and waits for it to answer. When another relay
    /// starts one at the same moment, the second `serve` finds the lock taken and exits, and
    /// both relays reach the first.
    fn start(address: &Path) -> io::Result<TcpStream> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["serve", "--socket"])
            .arg(address)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        keep_stdio_private();
        if command.creation_flags(DETACHED | BREAKAWAY).spawn().is_err() {
            command.creation_flags(DETACHED).spawn()?;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match connect(address) {
                Ok(stream) => return Ok(stream),
                Err(error) if Instant::now() > deadline => return Err(error),
                Err(_) => thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::io;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{Hub, home};

    pub(super) fn serve(_: Option<PathBuf>, _: Option<Duration>) -> io::Result<()> {
        Err(io::Error::other("the resident daemon needs a Unix or Windows host; use --stdio"))
    }

    /// Without a resident daemon this one lives only as long as this connection, but it still
    /// takes back what was saved before and saves what changes, as the resident daemon does.
    pub(super) fn relay(_: Option<PathBuf>) -> io::Result<()> {
        let home = home()?;
        let hub = Hub::open(&home)?;
        let served = hub.serve(io::stdin(), io::stdout()).map_err(io::Error::other);
        hub.shut_down(&home)?;
        served
    }
}
