//! The resident daemon outlives the connections that started it.

#![cfg(unix)]

use std::io::{BufReader, Write};
use std::process::{Command as Process, Stdio};

use maestro_protocol::{
    ClientMessage, Command, Reply, RequestId, ServerMessage, read_message, write_message,
};

/// Opens one `--stdio` connection, sends `command`, and returns the reply.
fn once(socket: &std::path::Path, command: Command) -> Reply {
    let mut relay = Process::new(env!("CARGO_BIN_EXE_maestro-daemon"))
        .arg("--stdio")
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = relay.stdin.take().unwrap();
    let mut stdout = BufReader::new(relay.stdout.take().unwrap());
    assert!(matches!(read_message(&mut stdout).unwrap(), Some(ServerMessage::Hello { .. })));
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
