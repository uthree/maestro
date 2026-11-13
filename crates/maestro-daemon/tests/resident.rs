//! The resident daemon outlives the connections that started it.

#![cfg(any(unix, windows))]

use std::io::{BufReader, Write};
use std::process::{Command as Process, Stdio};

use maestro_protocol::{
    ClientMessage, Command, PROTOCOL_VERSION, Reply, RequestId, ServerMessage, read_message,
    write_message,
};

/// What `--socket` names: the socket itself on Unix, the file holding the port on Windows.
#[cfg(unix)]
const SOCKET: &str = "daemon.sock";
#[cfg(windows)]
const SOCKET: &str = "daemon.address";

/// Stops the process whose pid is in `pid_file`.
fn kill(pid_file: &std::path::Path) {
    let pid = std::fs::read_to_string(pid_file).unwrap();
    #[cfg(unix)]
    Process::new("kill").arg(pid.trim()).status().unwrap();
    #[cfg(windows)]
    Process::new("taskkill").args(["/T", "/F", "/PID", pid.trim()]).output().unwrap();
}

/// Stops the daemon serving `socket`.
fn stop(socket: &std::path::Path) {
    kill(&socket.with_extension("pid"));
}

/// Opens one `--stdio` connection, sends `command`, and returns the reply.
fn once(socket: &std::path::Path, command: Command) -> Reply {
    let mut relay = Process::new(env!("CARGO_BIN_EXE_maestro-daemon"))
        .arg("--stdio")
        .arg("--socket")
        .arg(socket)
        // The daemon this starts keeps its record beside the socket, not in the real home.
        .env("MAESTRO_HOME", socket.with_file_name("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = relay.stdin.take().unwrap();
    let mut stdout = BufReader::new(relay.stdout.take().unwrap());
    let hello = read_message(&mut stdout).unwrap();
    assert_eq!(hello, Some(ServerMessage::Hello { protocol: PROTOCOL_VERSION }));
    write_message(&mut stdin, &ClientMessage { id: RequestId(1), command }).unwrap();
    stdin.flush().unwrap();
    let reply = loop {
        match read_message(&mut stdout).unwrap() {
            Some(ServerMessage::Response { result, .. }) => break result.unwrap(),
            Some(_) => {}
            None => panic!("the relay closed"),
        }
    };
    // Hanging up ends the relay, which drains whatever the daemon sent after the reply.
    drop(stdin);
    std::io::copy(&mut stdout, &mut std::io::sink()).unwrap();
    assert!(relay.wait().unwrap().success());
    reply
}

#[test]
fn state_survives_between_connections() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join(SOCKET);

    let created = once(&socket, Command::CreateWorkflow { title: "kept".into(), project: None });
    assert!(matches!(created, Reply::WorkflowCreated { .. }));
    let Reply::Workflows { workflows } = once(&socket, Command::ListWorkflows) else { panic!() };

    stop(&socket);
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0].title, "kept");
}

/// Two frontends opened at the same moment both find no daemon; they must still end up with
/// one, or each would see only its own work.
#[test]
fn frontends_starting_together_share_one_daemon() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join(SOCKET);

    std::thread::scope(|scope| {
        for title in ["a", "b", "c"] {
            let socket = &socket;
            scope.spawn(move || {
                let command = Command::CreateWorkflow { title: title.into(), project: None };
                assert!(matches!(once(socket, command), Reply::WorkflowCreated { .. }));
            });
        }
    });
    let Reply::Workflows { workflows } = once(&socket, Command::ListWorkflows) else { panic!() };

    stop(&socket);
    assert_eq!(workflows.len(), 3);
}

#[cfg(unix)]
#[test]
fn a_daemon_left_from_an_older_version_is_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join(SOCKET);
    // The old daemon: a socket that greets with the previous protocol, and a process to stop.
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let mut old = Process::new("sleep").arg("30").spawn().unwrap();
    std::fs::write(socket.with_extension("pid"), old.id().to_string()).unwrap();
    let serving = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        write_message(&mut stream, &ServerMessage::Hello { protocol: PROTOCOL_VERSION - 1 })
            .unwrap();
        // The socket answers until the process behind it is stopped.
        old.wait().unwrap()
    });

    let Reply::Workflows { workflows } = once(&socket, Command::ListWorkflows) else { panic!() };
    assert!(workflows.is_empty());
    assert!(!serving.join().unwrap().success(), "the old daemon was stopped");

    stop(&socket);
}

#[cfg(windows)]
#[test]
fn a_daemon_left_from_an_older_version_is_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join(SOCKET);
    // The old daemon: a port that greets with the previous protocol, and a process to stop.
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::fs::write(&socket, format!("{port} old-token\n")).unwrap();
    let mut old =
        Process::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(Stdio::null()).spawn().unwrap();
    std::fs::write(socket.with_extension("pid"), old.id().to_string()).unwrap();
    let serving = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        write_message(&mut stream, &ServerMessage::Hello { protocol: PROTOCOL_VERSION - 1 })
            .unwrap();
        // The port answers until the process behind it is stopped.
        old.wait().unwrap()
    });

    let Reply::Workflows { workflows } = once(&socket, Command::ListWorkflows) else { panic!() };
    assert!(workflows.is_empty());
    assert!(!serving.join().unwrap().success(), "the old daemon was stopped");

    stop(&socket);
}

/// A connection that does not know the token in the address file is closed unanswered.
#[cfg(windows)]
#[test]
fn a_caller_without_the_token_is_turned_away() {
    use std::io::Read;

    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join(SOCKET);
    once(&socket, Command::ListWorkflows);
    let text = std::fs::read_to_string(&socket).unwrap();
    let port: u16 = text.split_whitespace().next().unwrap().parse().unwrap();

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(b"wrong\n").unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).ok();

    stop(&socket);
    assert!(answer.is_empty(), "{}", String::from_utf8_lossy(&answer));
}

#[test]
fn an_idle_daemon_exits_and_the_next_one_remembers() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join(SOCKET);
    let mut daemon = Process::new(env!("CARGO_BIN_EXE_maestro-daemon"))
        .args(["serve", "--idle", "1", "--socket"])
        .arg(&socket)
        .env("MAESTRO_HOME", socket.with_file_name("home"))
        .spawn()
        .unwrap();
    while !socket.exists() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let created = once(&socket, Command::CreateWorkflow { title: "kept".into(), project: None });
    assert!(matches!(created, Reply::WorkflowCreated { .. }));
    // With no one connected and nothing running, it saves, cleans up and goes.
    assert!(daemon.wait().unwrap().success());
    assert!(!socket.exists());

    // The relay starts a new daemon, which has the workflow back.
    let Reply::Workflows { workflows } = once(&socket, Command::ListWorkflows) else { panic!() };
    stop(&socket);
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0].title, "kept");
}
