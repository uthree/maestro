//! A GUI and a TUI on one in-process daemon, end to end through the wire format.

use std::time::Duration;

use maestro_client::{Client, ClientError, Launch};
use maestro_core::{NodeDraft, NodeId, NodeOrigin, WorkflowId};
use maestro_daemon::Hub;
use maestro_protocol::{Command, CommandError, Event, Reply};

fn draft(title: &str) -> NodeDraft {
    NodeDraft {
        title: title.into(),
        instructions: String::new(),
        model: None,
        origin: NodeOrigin::User,
    }
}

#[test]
fn two_frontends_share_one_daemon() {
    let hub = Hub::new();
    let gui = Client::connect(&Launch::InProcess(hub.clone())).unwrap();
    let tui = Client::connect(&Launch::InProcess(hub)).unwrap();
    let tui_events = tui.take_events().unwrap();
    assert!(tui.take_events().is_none());

    assert_eq!(gui.request(Command::Ping).unwrap(), Reply::Pong);
    let Reply::WorkflowCreated { workflow } =
        gui.request(Command::CreateWorkflow { title: "release".into() }).unwrap()
    else {
        panic!("expected a new workflow");
    };
    gui.request(Command::AddNode { workflow, node: draft("build") }).unwrap();
    gui.request(Command::AddNode { workflow, node: draft("ship") }).unwrap();
    gui.request(Command::AddEdge { workflow, from: NodeId(1), to: NodeId(2) }).unwrap();

    // The TUI sees every edit the GUI made, in order, ending with the full graph.
    let seen: Vec<Event> =
        (0..4).map(|_| tui_events.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
    let Some(Event::WorkflowChanged { workflow: graph }) = seen.last() else {
        panic!("expected the graph last, got {seen:?}");
    };
    assert_eq!(graph.topological_order(), vec![NodeId(1), NodeId(2)]);

    let Reply::Workflows { workflows } = tui.request(Command::ListWorkflows).unwrap() else {
        panic!("expected a list");
    };
    assert_eq!(workflows[0].nodes, 2);
}

#[test]
fn refusals_reach_the_caller() {
    let client = Client::connect(&Launch::InProcess(Hub::new())).unwrap();
    let error = client.request(Command::GetWorkflow { workflow: WorkflowId(42) }).unwrap_err();
    assert!(matches!(
        error,
        ClientError::Refused(CommandError::UnknownWorkflow { workflow: WorkflowId(42) })
    ));
}
