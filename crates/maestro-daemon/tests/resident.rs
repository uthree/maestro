//! The resident daemon outlives the connections that started it.

#![cfg(unix)]

use std::io::{BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command as Process, Stdio};

use maestro_protocol::{
    ClientMessage, Command, PROTOCOL_VERSION, Reply, RequestId, ServerMessage, read_message,
    write_message,
};

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
    let socket = directory.path().join("daemon.sock");

    let created = once(&socket, Command::CreateWorkflow { title: "kept".into() });
    assert!(matches!(created, Reply::WorkflowCreated { .. }));
    let Reply::Workflows { workflows } = once(&socket, Command::ListWorkflows) else { panic!() };

    let pid = std::fs::read_to_string(socket.with_extension("pid")).unwrap();
    Process::new("kill").arg(pid.trim()).status().unwrap();
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0].title, "kept");
}

#[test]
fn a_daemon_left_from_an_older_version_is_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("daemon.sock");
    // The old daemon: a socket that greets with the previous protocol, and a process to stop.
    let listener = UnixListener::bind(&socket).unwrap();
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

    let pid = std::fs::read_to_string(socket.with_extension("pid")).unwrap();
    Process::new("kill").arg(pid.trim()).status().unwrap();
}
