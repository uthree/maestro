//! The window's root view: what it shows, and every request it makes.
//!
//! The view holds a [`Mirror`] of the daemon and redraws from it. Every change the user makes is
//! a [`Command`] sent from a background task; the result comes back as events, which every
//! connected frontend sees, so this window never edits its copy directly.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use atelier::{
    ActiveTheme, Badge, Button, ControlSize, IconName, Label, TextColor, TextSize, TitleBar, Tone,
    WindowFrame,
};
use futures::StreamExt;
use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::input::InputState;
use maestro_client::mirror::Notice;
use maestro_client::{Client, Launch, Mirror};
use maestro_core::{
    AgentId, AgentRole, Difficulty, Effort, Host, ModelChoice, ModelGrade, NodeDraft, NodeEdit,
    NodeId, NodeOrigin, NodeStatus, ProviderKind, Workflow, WorkflowId, WorkflowTarget, Workspace,
};
use maestro_protocol::{AgentStatus, Command, Event, Reply};

use crate::graph::Canvas;
use crate::{input, link};

/// Narrowest window that still shows the sidebar.
const SIDEBAR_MIN_WINDOW: f32 = 1180.;

/// The repository commanders read and workflows run in, on the daemon's machine.
pub struct Project {
    /// Its main checkout, once chosen.
    pub repository: Option<PathBuf>,
    /// The branch work starts from and pull requests merge into.
    pub base: String,
}

impl Project {
    /// A project as given on the command line.
    pub fn new(repository: Option<PathBuf>, base: String) -> Self {
        Self { repository, base }
    }

    /// Where a workflow's workers run: worktrees beside the main checkout, so they never show
    /// up in it as untracked files.
    pub fn target(&self) -> Option<WorkflowTarget> {
        let repository = self.repository.clone()?;
        let mut worktrees = repository.clone().into_os_string();
        worktrees.push("-worktrees");
        Some(WorkflowTarget {
            host: Host::Local,
            repository,
            base: self.base.clone(),
            worktrees: worktrees.into(),
            publish: false,
        })
    }
}

/// How the window stands with the daemon.
pub enum Connection {
    /// Starting it, or reaching it over SSH.
    Connecting,
    /// Connected.
    Ready(Arc<Client>),
    /// It could not be reached, or went away.
    Lost(String),
}

/// The text fields, kept as entities so their caret and selection outlive a frame.
pub struct Fields {
    /// What the user writes to the commander.
    pub composer: Entity<InputState>,
    /// What the user writes to the worker on the selected node.
    pub node_message: Entity<InputState>,
    /// The selected node's title.
    pub title: Entity<InputState>,
    /// The selected node's instructions.
    pub instructions: Entity<InputState>,
    /// The project's repository path.
    pub repository: Entity<InputState>,
    /// The project's base branch.
    pub base: Entity<InputState>,
}

/// The window.
pub struct Maestro {
    pub(crate) launch: Launch,
    pub(crate) connection: Connection,
    pub(crate) mirror: Mirror,
    pub(crate) project: Project,
    /// The workflow on the canvas.
    pub(crate) workflow: Option<WorkflowId>,
    /// The node in the inspector.
    pub(crate) selected: Option<NodeId>,
    /// Which node the title and instruction fields were filled from.
    pub(crate) editing: Option<(WorkflowId, NodeId)>,
    /// The commander in the panel.
    pub(crate) conversation: Option<AgentId>,
    pub(crate) panel_open: bool,
    pub(crate) sidebar_open: bool,
    /// The inspector's tab: the node's details or its worker.
    pub(crate) node_tab: usize,
    /// The CLI a new commander runs on.
    pub(crate) provider: ProviderKind,
    pub(crate) canvas: Canvas,
    /// Reasoning blocks and tool calls the user has opened.
    pub(crate) expanded: HashSet<SharedString>,
    /// Ratings given in this window, to show which star was picked.
    pub(crate) ratings: HashMap<(WorkflowId, NodeId), u8>,
    /// The last request that failed, until dismissed.
    pub(crate) error: Option<String>,
    /// How many notices have been seen, so a new one can be announced.
    pub(crate) notices_seen: usize,
    pub(crate) fields: Fields,
}

impl Maestro {
    /// Opens the window's view and starts connecting.
    pub fn new(
        launch: Launch,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let repository = project.repository.as_ref().map(|path| path.display().to_string());
        let fields = Fields {
            composer: input::area("Describe what to build…", "", 8, window, cx),
            node_message: input::area("Ask this worker for changes…", "", 6, window, cx),
            title: input::line("Step name", "", window, cx),
            instructions: input::area("What the worker should do", "", 12, window, cx),
            repository: input::line(
                "/path/to/repository",
                &repository.unwrap_or_default(),
                window,
                cx,
            ),
            base: input::line("main", &project.base, window, cx),
        };
        let mut view = Self {
            launch,
            connection: Connection::Connecting,
            mirror: Mirror::default(),
            project,
            workflow: None,
            selected: None,
            editing: None,
            conversation: None,
            panel_open: true,
            sidebar_open: true,
            node_tab: 0,
            provider: ProviderKind::ClaudeCode,
            canvas: Canvas::default(),
            expanded: HashSet::new(),
            ratings: HashMap::new(),
            error: None,
            notices_seen: 0,
            fields,
        };
        view.connect(cx);
        view
    }

    /// Connects in the background, then follows the daemon's events until it goes away.
    pub(crate) fn connect(&mut self, cx: &mut Context<Self>) {
        self.connection = Connection::Connecting;
        let launch = self.launch.clone();
        cx.spawn(async move |this, cx| {
            let opened = cx.background_executor().spawn(async move { link::open(&launch) }).await;
            let events = this
                .update(cx, |this, cx| {
                    cx.notify();
                    match opened {
                        Ok(opened) => {
                            this.connection = Connection::Ready(opened.client);
                            this.mirror = opened.mirror;
                            this.notices_seen = this.mirror.notices.len();
                            this.workflow = this.mirror.workflows.keys().next_back().copied();
                            this.conversation = this.mirror.commanders().last().map(|(id, _)| id);
                            Some(opened.events)
                        }
                        Err(error) => {
                            this.connection = Connection::Lost(link::explain(&error, &this.launch));
                            None
                        }
                    }
                })
                .ok()
                .flatten();
            let Some(mut events) = events else { return };
            while let Some(event) = events.next().await {
                if this.update(cx, |this, cx| this.receive(event, cx)).is_err() {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.connection =
                    Connection::Lost("The daemon disconnected. Try again to reconnect.".into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn receive(&mut self, event: Event, cx: &mut Context<Self>) {
        self.mirror.apply(&event);
        match event {
            // A plan from the conversation on screen is what the user wants to see next.
            Event::Planned { agent, workflow } if Some(agent) == self.conversation => {
                self.show_workflow(workflow);
            }
            Event::WorkflowChanged { workflow } if self.workflow.is_none() => {
                self.show_workflow(workflow.id);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Sends a command from a background task and hands its reply to `then`. A refusal is shown
    /// in the window's footer.
    pub(crate) fn request(
        &mut self,
        command: Command,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Reply, &mut Context<Self>) + 'static,
    ) {
        let Connection::Ready(client) = &self.connection else {
            self.error = Some("Not connected to the daemon.".into());
            cx.notify();
            return;
        };
        let client = client.clone();
        cx.spawn(async move |this, cx| {
            let result =
                cx.background_executor().spawn(async move { client.request(command) }).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(reply) => then(this, reply, cx),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Sends a command whose effect arrives as events.
    pub(crate) fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        self.request(command, cx, |_, _, _| {});
    }

    // What is on screen.

    pub(crate) fn current(&self) -> Option<&Workflow> {
        self.mirror.workflows.get(&self.workflow?)
    }

    pub(crate) fn show_workflow(&mut self, workflow: WorkflowId) {
        if self.workflow != Some(workflow) {
            self.workflow = Some(workflow);
            self.selected = None;
            self.canvas.reset();
        }
    }

    /// Selects a node; the edit fields are filled from it at the next frame.
    pub(crate) fn select(&mut self, node: Option<NodeId>) {
        self.selected = node;
    }

    /// Fills the edit fields from the selected node once it is in the copy, if they hold
    /// another node's text.
    fn fill_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(workflow), Some(node)) = (self.workflow, self.selected) else { return };
        if self.editing == Some((workflow, node)) {
            return;
        }
        let Some(found) = self.current().and_then(|flow| flow.node(node)) else { return };
        let (title, instructions) = (found.title.clone(), found.instructions.clone());
        // A worker that is busy or waiting is what the user came to see.
        self.node_tab = match found.status {
            _ if found.agent.is_none() => 0,
            NodeStatus::Running | NodeStatus::AwaitingUser => 1,
            _ => self.node_tab,
        };
        self.editing = Some((workflow, node));
        self.fields.title.update(cx, |state, cx| state.set_value(title, window, cx));
        self.fields.instructions.update(cx, |state, cx| state.set_value(instructions, window, cx));
    }

    /// Reads the project fields into the project.
    fn read_project(&mut self, cx: &Context<Self>) {
        let repository = input::text(&self.fields.repository, cx);
        let repository = repository.trim();
        self.project.repository = (!repository.is_empty()).then(|| PathBuf::from(repository));
        let base = input::text(&self.fields.base, cx);
        let base = base.trim();
        self.project.base = if base.is_empty() { "main".into() } else { base.into() };
    }

    // Requests.

    pub(crate) fn send_to_commander(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = input::text(&self.fields.composer, cx);
        if text.trim().is_empty() {
            return;
        }
        let alive = self
            .conversation
            .and_then(|agent| self.mirror.agents.get(&agent).map(|view| (agent, view.status)))
            .filter(|(_, status)| *status != AgentStatus::Exited);
        if let Some((agent, _)) = alive {
            input::clear(&self.fields.composer, window, cx);
            self.command(Command::SendToAgent { agent, text }, cx);
            return;
        }
        self.read_project(cx);
        let Some(target) = self.project.target() else {
            self.error = Some("Choose the repository the commander should plan for.".into());
            cx.notify();
            return;
        };
        input::clear(&self.fields.composer, window, cx);
        let command = Command::StartCommander {
            model: ModelChoice {
                provider: self.provider,
                model: String::new(),
                grade: ModelGrade::Standard,
                effort: Effort::Medium,
            },
            workspace: Workspace {
                host: target.host.clone(),
                repository: target.repository.clone(),
                worktree: None,
            },
            target: Some(target),
            prompt: text,
        };
        self.request(command, cx, |this, reply, _| {
            if let Reply::AgentStarted { agent } = reply {
                this.conversation = Some(agent);
            }
        });
    }

    pub(crate) fn new_conversation(&mut self, cx: &mut Context<Self>) {
        self.conversation = None;
        self.panel_open = true;
        cx.notify();
    }

    pub(crate) fn stop_commander(&mut self, cx: &mut Context<Self>) {
        if let Some(agent) = self.conversation {
            self.command(Command::InterruptAgent { agent }, cx);
        }
    }

    pub(crate) fn answer(
        &mut self,
        agent: AgentId,
        request: String,
        allow: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.mirror.agents.get_mut(&agent) {
            view.transcript.answered(&request, allow);
        }
        self.command(Command::AnswerPermission { agent, request, allow }, cx);
    }

    pub(crate) fn new_workflow(&mut self, cx: &mut Context<Self>) {
        self.request(
            Command::CreateWorkflow { title: "Untitled workflow".into() },
            cx,
            |this, reply, _| {
                if let Reply::WorkflowCreated { workflow } = reply {
                    this.show_workflow(workflow);
                }
            },
        );
    }

    /// Runs the workflow on screen, first giving it the project as its target if it has none.
    pub(crate) fn run(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.current() else { return };
        let workflow = flow.id;
        if flow.target.is_some() {
            return self.command(Command::RunWorkflow { workflow }, cx);
        }
        self.read_project(cx);
        let Some(target) = self.project.target() else {
            self.error = Some("Choose the repository this workflow should run in.".into());
            return;
        };
        self.request(
            Command::SetWorkflowTarget { workflow, target: Some(target) },
            cx,
            move |this, _, cx| {
                this.command(Command::RunWorkflow { workflow }, cx);
            },
        );
    }

    pub(crate) fn publish(&mut self, cx: &mut Context<Self>) {
        let Some(workflow) = self.workflow else { return };
        self.command(Command::PublishWorkflow { workflow }, cx);
    }

    pub(crate) fn set_publish(&mut self, publish: bool, cx: &mut Context<Self>) {
        let Some(flow) = self.current() else { return };
        let workflow = flow.id;
        let target = flow.target.clone().or_else(|| {
            self.read_project(cx);
            self.project.target()
        });
        let Some(mut target) = target else {
            self.error = Some("Choose the repository this workflow should run in.".into());
            return;
        };
        target.publish = publish;
        self.command(Command::SetWorkflowTarget { workflow, target: Some(target) }, cx);
    }

    pub(crate) fn save_project_target(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.current() else { return };
        let workflow = flow.id;
        let publish = flow.target.as_ref().is_some_and(|target| target.publish);
        self.read_project(cx);
        let Some(mut target) = self.project.target() else {
            self.error = Some("Choose the repository this workflow should run in.".into());
            return;
        };
        target.publish = publish;
        self.command(Command::SetWorkflowTarget { workflow, target: Some(target) }, cx);
    }

    pub(crate) fn add_node(&mut self, cx: &mut Context<Self>) {
        let Some(workflow) = self.workflow else { return };
        let node = NodeDraft {
            title: "New step".into(),
            instructions: String::new(),
            model: None,
            difficulty: Some(Difficulty::Moderate),
            origin: NodeOrigin::User,
        };
        self.request(Command::AddNode { workflow, node }, cx, move |this, reply, _| {
            if let Reply::NodeAdded { node } = reply {
                this.canvas.place_new(workflow, node);
                this.select(Some(node));
            }
        });
    }

    pub(crate) fn remove_node(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let Some(workflow) = self.workflow else { return };
        if self.selected == Some(node) {
            self.selected = None;
        }
        self.command(Command::RemoveNode { workflow, node }, cx);
    }

    pub(crate) fn connect_nodes(&mut self, from: NodeId, to: NodeId, cx: &mut Context<Self>) {
        let Some(workflow) = self.workflow else { return };
        if from != to {
            self.command(Command::AddEdge { workflow, from, to }, cx);
        }
    }

    pub(crate) fn disconnect_nodes(&mut self, from: NodeId, to: NodeId, cx: &mut Context<Self>) {
        let Some(workflow) = self.workflow else { return };
        self.command(Command::RemoveEdge { workflow, from, to }, cx);
    }

    pub(crate) fn edit_node(&mut self, node: NodeId, edit: NodeEdit, cx: &mut Context<Self>) {
        let Some(workflow) = self.workflow else { return };
        self.command(Command::EditNode { workflow, node, edit }, cx);
    }

    /// Saves the title and instructions fields to the selected node.
    pub(crate) fn save_node(&mut self, cx: &mut Context<Self>) {
        let Some(node) = self.selected.and_then(|id| self.current()?.node(id)) else { return };
        let title = input::text(&self.fields.title, cx);
        let instructions = input::text(&self.fields.instructions, cx);
        let edit = NodeEdit {
            title: (title != node.title).then_some(title),
            instructions: (instructions != node.instructions).then_some(instructions),
            ..NodeEdit::default()
        };
        let id = node.id;
        self.edit_node(id, edit, cx);
    }

    pub(crate) fn send_to_node(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(workflow), Some(node)) = (self.workflow, self.selected) else { return };
        let text = input::text(&self.fields.node_message, cx);
        if text.trim().is_empty() {
            return;
        }
        input::clear(&self.fields.node_message, window, cx);
        self.command(Command::SendToNode { workflow, node, text }, cx);
    }

    pub(crate) fn rate(&mut self, node: NodeId, rating: u8, cx: &mut Context<Self>) {
        let Some(workflow) = self.workflow else { return };
        self.request(Command::RateNode { workflow, node, rating }, cx, move |this, _, _| {
            this.ratings.insert((workflow, node), rating);
        });
    }

    pub(crate) fn interrupt(&mut self, agent: AgentId, cx: &mut Context<Self>) {
        self.command(Command::InterruptAgent { agent }, cx);
    }

    // Drawing.

    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let flow = self.current();
        let title: SharedString = flow.map_or("Maestro".into(), |flow| flow.title.clone().into());
        let place = link::describe(&self.launch);
        let subtitle = match &self.project.repository {
            Some(path) => format!("{} · {place}", path.display()),
            None => place,
        };
        let running = flow.is_some_and(|flow| {
            flow.nodes()
                .any(|node| matches!(node.status, NodeStatus::Running | NodeStatus::AwaitingUser))
        });
        let finished = flow.is_some_and(|flow| {
            flow.nodes().next().is_some()
                && flow.nodes().all(|node| node.status == NodeStatus::Succeeded)
        });
        let waiting = flow.map_or(0, |flow| {
            flow.nodes().filter(|node| node.status == NodeStatus::AwaitingUser).count()
        });
        let failed = flow.map_or(0, |flow| {
            flow.nodes().filter(|node| node.status == NodeStatus::Failed).count()
        });
        let ready = flow.is_some_and(|flow| flow.nodes().next().is_some()) && !running && !finished;
        let conversation = self.conversation.and_then(|agent| self.mirror.agents.get(&agent));
        let status = conversation.map_or(atelier::agent::AgentStatus::Idle, |view| {
            crate::panels::agent_status(view.status, !view.transcript.entries.is_empty())
        });

        TitleBar::new()
            .leading(
                Button::icon_only("toggle-sidebar", IconName::SidebarLeft)
                    .tooltip(if self.sidebar_open { "Hide sidebar" } else { "Show sidebar" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_open = !this.sidebar_open;
                        cx.notify();
                    })),
            )
            .title(title)
            .subtitle(subtitle)
            .when(flow.is_some(), |bar| {
                bar.child(if finished {
                    Button::new("publish", "Open pull request")
                        .primary()
                        .icon(IconName::ArrowUp)
                        .size(ControlSize::Small)
                        .on_click(cx.listener(|this, _, _, cx| this.publish(cx)))
                } else {
                    Button::new("run", "Run")
                        .primary()
                        .icon(IconName::Play)
                        .size(ControlSize::Small)
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, _, cx| this.run(cx)))
                })
            })
            .when(waiting > 0, |bar| {
                bar.child(
                    Badge::new(format!("{waiting} waiting for you"))
                        .tone(Tone::Warning)
                        .icon(IconName::Alert),
                )
            })
            .when(failed > 0, |bar| {
                bar.child(
                    Badge::new(format!("{failed} failed")).tone(Tone::Danger).icon(IconName::Alert),
                )
            })
            .trailing(
                atelier::agent::AgentToggle::new("commander-toggle", self.panel_open, status)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.panel_open = !this.panel_open;
                        cx.notify();
                    })),
            )
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let place = link::describe(&self.launch);
        let (connection, tone) = match &self.connection {
            Connection::Connecting => (format!("Connecting to {place}…"), Tone::Neutral),
            Connection::Ready(_) => (format!("Connected to {place}"), Tone::Success),
            Connection::Lost(_) => (format!("Not connected to {place}"), Tone::Danger),
        };
        let working = self
            .mirror
            .agents
            .values()
            .filter(|view| {
                view.spec.role == AgentRole::Worker && view.status == AgentStatus::Working
            })
            .count();
        let notice = self.mirror.notices.last().map(|notice| match notice {
            Notice::PlanRejected { message, .. } => {
                (format!("Plan not used: {message}"), Tone::Warning)
            }
            Notice::NodeFailed { node, message, .. } => {
                (format!("{node} failed: {message}"), Tone::Danger)
            }
            Notice::Published { number, url, .. } => {
                (format!("Opened pull request #{number}: {url}"), Tone::Success)
            }
        });
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme.spacing.md)
            .h(px(26.))
            .px(theme.spacing.md)
            .bg(theme.colors.chrome)
            .border_t_1()
            .border_color(theme.colors.border)
            .child(Badge::new(connection).tone(tone).dot())
            .when(working > 0, |row| {
                row.child(
                    Label::new(format!(
                        "{working} worker{} running",
                        if working == 1 { "" } else { "s" }
                    ))
                    .size(TextSize::XSmall)
                    .color(TextColor::Muted),
                )
            })
            .child(div().flex_1())
            .children(notice.filter(|_| self.error.is_none()).map(|(text, tone)| {
                div().min_w_0().child(
                    Label::new(text).size(TextSize::XSmall).color(text_color(tone)).truncate(),
                )
            }))
            .children(self.error.clone().map(|error| {
                div()
                    .flex()
                    .items_center()
                    .gap(theme.spacing.sm)
                    .min_w_0()
                    .child(
                        Label::new(error)
                            .size(TextSize::XSmall)
                            .color(TextColor::Danger)
                            .truncate(),
                    )
                    .child(
                        Button::new("dismiss-error", "Dismiss")
                            .ghost()
                            .size(ControlSize::Small)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.error = None;
                                cx.notify();
                            })),
                    )
            }))
    }
}

fn text_color(tone: Tone) -> TextColor {
    match tone {
        Tone::Success => TextColor::Success,
        Tone::Warning => TextColor::Warning,
        Tone::Danger => TextColor::Danger,
        _ => TextColor::Muted,
    }
}

impl Render for Maestro {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.fill_fields(window, cx);
        let theme = cx.theme().clone();
        let width = window.viewport_size().width;
        let sidebar = self.sidebar_open && width >= px(SIDEBAR_MIN_WINDOW);
        let panel_width = if width >= px(SIDEBAR_MIN_WINDOW) { 380. } else { 320. };

        let body = match &self.connection {
            Connection::Ready(_) => div()
                .flex()
                .flex_1()
                .min_h_0()
                .when(sidebar, |row| row.child(self.sidebar(cx)))
                .child(match self.workflow {
                    Some(_) => self.graph(cx).into_any_element(),
                    None => self.welcome(cx).into_any_element(),
                })
                .map(|row| {
                    if self.panel_open {
                        row.child(self.commander_panel(panel_width, cx).into_any_element())
                    } else if self.workflow.is_some() {
                        row.child(self.inspector(cx).into_any_element())
                    } else {
                        row
                    }
                })
                .into_any_element(),
            Connection::Connecting => self.connecting(cx).into_any_element(),
            Connection::Lost(reason) => self.lost(reason.clone(), cx).into_any_element(),
        };

        WindowFrame::new()
            .font(theme.typography.ui.clone())
            .text_color(theme.colors.text)
            .size_full()
            .flex()
            .flex_col()
            .child(self.title_bar(cx))
            .child(body)
            .child(self.footer(cx))
    }
}
