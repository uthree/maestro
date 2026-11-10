//! The workflow canvas: Atelier's `NodeCanvas`, filled from the workflow in the mirror.
//!
//! A workflow has one kind of wire, "waits for", into an input that takes many wires, so every
//! port shares the first port colour and the legend names it. The canvas applies a gesture at
//! once and reports it; the report becomes a command, and the daemon's answer comes back as a
//! changed workflow that the canvas is filled from again. Where a node sits is this window's own
//! business: the daemon's graph has no positions, so the layout comes from [`Workflow::depths`]
//! and a node the user drags keeps its place here.

use std::collections::HashMap;

use atelier::graph::{GraphNode, NodeCanvas, NodeCanvasEvent, NodeId as CanvasId, NodeState};
use atelier::{
    ActiveTheme, Badge, Button, ButtonVariant, ControlSize, Divider, EmptyState, IconName, Label,
    StatusDot, TextColor, TextSize, Tone,
};
use gpui::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, Pixels, Point, Styled, div,
    point, prelude::*, px,
};
use maestro_core::{
    ModelChoice, Node, NodeEdit, NodeId, NodeOrigin, NodeStatus, Workflow, WorkflowId,
};

use crate::app::Maestro;
use crate::i18n::{self, tr};
use crate::trf;

/// A node's width on the canvas.
const NODE_WIDTH: f32 = 248.;
/// Horizontal distance between the left edges of two columns.
const COLUMN: f32 = NODE_WIDTH + 104.;
/// Vertical distance between the tops of two nodes in one column.
const ROW: f32 = 196.;

/// The canvas and what this window keeps about it.
pub struct Canvas {
    /// Atelier's canvas, which draws the graph and takes the gestures.
    pub view: Entity<NodeCanvas>,
    /// Nodes the user moved, or that were added where the user could see them.
    placed: HashMap<(WorkflowId, NodeId), Point<Pixels>>,
    /// The workflow the canvas was last filled from.
    shown: Option<Workflow>,
    /// The canvas's id for each node of [`Canvas::shown`], and back.
    ids: HashMap<NodeId, CanvasId>,
    nodes: HashMap<CanvasId, NodeId>,
    /// Whether to pan back to the start the next time the canvas is shown.
    reset: bool,
}

impl Canvas {
    /// An empty canvas, whose gestures `Maestro` hears.
    pub fn new(cx: &mut Context<Maestro>) -> Self {
        let view = cx.new(|_| {
            NodeCanvas::new("workflow").port_types([tr("Waits for", "待機関係")]).without_legend()
        });
        cx.subscribe(&view, |this: &mut Maestro, _, event: &NodeCanvasEvent, cx| {
            this.canvas_event(event, cx)
        })
        .detach();
        Self {
            view,
            placed: HashMap::new(),
            shown: None,
            ids: HashMap::new(),
            nodes: HashMap::new(),
            reset: false,
        }
    }

    /// Back to the start of the graph.
    pub fn reset(&mut self, cx: &mut App) {
        self.view.update(cx, |canvas, cx| canvas.reset_view(cx));
    }

    /// Back to the start of the graph the next time it is shown.
    pub fn reset_later(&mut self) {
        self.reset = true;
    }

    /// Puts a new node near the top-left of what is visible.
    pub fn place_new(&mut self, workflow: WorkflowId, node: NodeId, cx: &App) {
        let step = (node.0 % 6) as f32 * 24.;
        let at = point(px(120. + step), px(80. + step)) - self.view.read(cx).pan();
        self.placed.insert((workflow, node), at);
    }

    /// Where every node of `flow` sits, before panning.
    fn layout(&self, flow: &Workflow) -> HashMap<NodeId, Point<Pixels>> {
        let depths = flow.depths();
        let mut rows: HashMap<usize, usize> = HashMap::new();
        flow.topological_order()
            .into_iter()
            .map(|id| {
                let column = depths.get(&id).copied().unwrap_or(0);
                let row = rows.entry(column).or_default();
                let at = point(px(column as f32 * COLUMN), px(*row as f32 * ROW));
                *row += 1;
                (id, self.placed.get(&(flow.id, id)).copied().unwrap_or(at))
            })
            .collect()
    }

    /// Fills the canvas from `flow` when it changed since the last time, and selects `selected`.
    pub fn show(&mut self, flow: Option<&Workflow>, selected: Option<NodeId>, cx: &mut App) {
        if std::mem::take(&mut self.reset) {
            self.reset(cx);
        }
        if self.shown.as_ref() != flow {
            self.shown = flow.cloned();
            let layout = flow.map(|flow| self.layout(flow)).unwrap_or_default();
            let (old, mut ids) = (std::mem::take(&mut self.nodes), HashMap::new());
            self.view.update(cx, |canvas, cx| {
                for id in old.into_keys() {
                    canvas.remove(id, cx);
                }
                let Some(flow) = flow else { return };
                for node in flow.nodes() {
                    let at = layout.get(&node.id).copied().unwrap_or_default();
                    ids.insert(node.id, canvas.add(graph_node(node, at), cx));
                }
                for edge in flow.edges() {
                    if let (Some(from), Some(to)) = (ids.get(&edge.from), ids.get(&edge.to)) {
                        canvas.connect(atelier::graph::Wire::new(*from, 0, *to, 0), cx);
                    }
                }
            });
            self.nodes = ids.iter().map(|(node, id)| (*id, *node)).collect();
            self.ids = ids;
        }
        let wanted = selected.and_then(|node| self.ids.get(&node).copied());
        if self.view.read(cx).selected() != wanted {
            self.view.update(cx, |canvas, cx| canvas.select(wanted, cx));
        }
    }

    /// The workflow's node the canvas calls `id`.
    fn node(&self, id: CanvasId) -> Option<NodeId> {
        self.nodes.get(&id).copied()
    }
}

/// How the canvas shows a node's status.
fn state(node: &Node) -> NodeState {
    match node.status {
        NodeStatus::Pending => NodeState::Idle,
        NodeStatus::Running => NodeState::Working,
        NodeStatus::AwaitingUser => NodeState::NeedsAnswer,
        NodeStatus::AwaitingChoice => NodeState::NeedsChoice,
        NodeStatus::Succeeded => NodeState::Done(None),
        NodeStatus::Failed => NodeState::Failed,
    }
}

/// A step the commander planned is the AI's draft until the user keeps it or it runs.
fn draft(node: &Node) -> bool {
    node.origin == NodeOrigin::Commander && node.status == NodeStatus::Pending
}

/// A workflow node as the canvas draws it: its title and status in the header, the wire ports,
/// then its difficulty, model, first line of instructions and branch.
fn graph_node(node: &Node, at: Point<Pixels>) -> GraphNode {
    let difficulty = node.difficulty.map(i18n::difficulty);
    let delegate = node.delegate;
    let model = model_label(node);
    let instructions: String = if node.instructions.is_empty() {
        tr("No instructions yet", "指示はまだありません").into()
    } else {
        node.instructions.lines().next().unwrap_or_default().into()
    };
    let branch = node.branch.clone();
    GraphNode::new(node.title.clone())
        .at(f32::from(at.x), f32::from(at.y))
        .width(px(NODE_WIDTH))
        .input_many(tr("Waits for", "待機先"), 0)
        .output(tr("Then", "次へ"), 0)
        .state(state(node))
        .by_agent(draft(node))
        .body(move |_, _, cx| {
            let theme = cx.theme();
            div()
                .flex()
                .flex_col()
                .gap(theme.spacing.xs)
                .px(theme.spacing.md)
                .py(theme.spacing.sm)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme.spacing.sm)
                        .children(difficulty.map(Badge::new))
                        // A lead plans it as a workflow of its own.
                        .when(delegate, |row| {
                            row.child(Badge::new(tr("Lead", "リード")).icon(IconName::Sliders))
                        })
                        .child(
                            div().flex_1().min_w_0().child(
                                Label::new(model.clone())
                                    .size(TextSize::XSmall)
                                    .color(TextColor::Muted)
                                    .truncate(),
                            ),
                        ),
                )
                .child(
                    Label::new(instructions.clone())
                        .size(TextSize::XSmall)
                        .color(TextColor::Faint)
                        .truncate(),
                )
                .children(branch.clone().map(|branch| {
                    Label::new(branch)
                        .size(TextSize::XSmall)
                        .mono()
                        .color(TextColor::Faint)
                        .truncate()
                }))
                .into_any_element()
        })
}

/// A status in words and a mark, never colour alone.
pub fn status_label(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Pending => tr("Waiting", "待機中"),
        NodeStatus::Running => tr("Working", "作業中"),
        NodeStatus::AwaitingUser => tr("Needs you", "要対応"),
        NodeStatus::AwaitingChoice => tr("Pick one", "選択待ち"),
        NodeStatus::Succeeded => tr("Done", "完了"),
        NodeStatus::Failed => tr("Failed", "失敗"),
    }
}

/// The badge tone of a status.
pub fn status_tone(status: NodeStatus) -> Tone {
    match status {
        NodeStatus::Pending => Tone::Neutral,
        NodeStatus::Running => Tone::Accent,
        NodeStatus::AwaitingUser | NodeStatus::AwaitingChoice => Tone::Warning,
        NodeStatus::Succeeded => Tone::Success,
        NodeStatus::Failed => Tone::Danger,
    }
}

/// The model a node runs on, or will.
pub fn model_label(node: &Node) -> String {
    if node.model.is_none() && !node.contenders.is_empty() {
        let names: Vec<String> =
            node.contenders.iter().map(|contender| choice_label(&contender.model)).collect();
        return trf!("Compared: {}", "比較中: {}", names.join(" vs "));
    }
    match &node.model {
        Some(choice) if choice.model.is_empty() => {
            format!("{} · {}", choice.provider.label(), i18n::effort(choice.effort))
        }
        Some(choice) => format!("{} · {}", choice.model, i18n::effort(choice.effort)),
        None => tr("Chosen when it runs", "実行時に選択").into(),
    }
}

/// A model in a few words: its name, or its provider's when it is the CLI's default.
pub fn choice_label(choice: &ModelChoice) -> String {
    if choice.model.is_empty() { choice.provider.label().into() } else { choice.model.clone() }
}

impl Maestro {
    /// What the person did on the canvas, which has already applied it, as a command.
    fn canvas_event(&mut self, event: &NodeCanvasEvent, cx: &mut Context<Self>) {
        let node = |id| self.canvas.node(id);
        match *event {
            NodeCanvasEvent::Selected(id) => self.select(id.and_then(node)),
            NodeCanvasEvent::Moved(id) => {
                let (Some(workflow), Some(moved)) = (self.workflow, node(id)) else { return };
                if let Some(at) = self.canvas.view.read(cx).node(id).map(|node| node.position) {
                    self.canvas.placed.insert((workflow, moved), at);
                }
            }
            NodeCanvasEvent::Connected(wire) => {
                if let (Some(from), Some(to)) = (node(wire.from), node(wire.to)) {
                    self.connect_nodes(from, to, cx);
                }
            }
            NodeCanvasEvent::Disconnected(wire) => {
                if let (Some(from), Some(to)) = (node(wire.from), node(wire.to)) {
                    self.disconnect_nodes(from, to, cx);
                }
            }
            // The canvas wires an undone draft's neighbours together; the daemon only removes
            // it, and the canvas is filled again from what it says.
            NodeCanvasEvent::Removed(id) | NodeCanvasEvent::Undone(id) => {
                if let Some(removed) = node(id) {
                    self.remove_node(removed, cx);
                }
            }
            NodeCanvasEvent::Kept(id) => {
                if let Some(kept) = node(id) {
                    self.edit_node(kept, NodeEdit::default(), cx);
                }
            }
            NodeCanvasEvent::CommentPressed { .. } => {}
        }
        cx.notify();
    }

    fn legend(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        div()
            .absolute()
            .left(theme.spacing.lg)
            .bottom(theme.spacing.lg)
            .flex()
            .items_center()
            .gap(theme.spacing.md)
            .px(theme.spacing.md)
            .py(theme.spacing.xs)
            .rounded(theme.shape.radius_md)
            .bg(theme.colors.overlay)
            .border_1()
            .border_color(theme.colors.border)
            .shadow(theme.elevation.medium.clone())
            .child(
                Button::new("add-node", tr("Add step", "ステップを追加"))
                    .ghost()
                    .icon(IconName::Plus)
                    .size(ControlSize::Small)
                    .on_click(cx.listener(|this, _, _, cx| this.add_node(cx))),
            )
            .child(
                Button::new("reset-view", tr("Reset view", "表示をリセット"))
                    .ghost()
                    .size(ControlSize::Small)
                    .on_click(cx.listener(|this, _, _, cx| this.canvas.reset(cx))),
            )
            .child(Divider::vertical())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme.spacing.xs)
                    .child(StatusDot::new(theme.colors.ports[0]))
                    .child(
                        Label::new(tr(
                            "Waits for · drag from a step's “Then” to another step's “Waits for”",
                            "待機関係 · ステップの「次へ」から別のステップの「待機先」へドラッグ",
                        ))
                        .size(TextSize::XSmall)
                        .color(TextColor::Muted),
                    ),
            )
    }

    /// Fills the canvas from the workflow on screen, before a frame is drawn.
    pub(crate) fn show_graph(&mut self, cx: &mut Context<Self>) {
        let flow = self.current().cloned();
        self.canvas.show(flow.as_ref(), self.selected, cx);
    }

    /// The canvas for the workflow on screen.
    pub(crate) fn graph(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(flow) = self.current() else {
            return div().flex_1().into_any_element();
        };
        let empty = flow.nodes().next().is_none();
        div()
            .id("graph")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .child(div().absolute().inset_0().child(self.canvas.view.clone()))
            .when(empty, |graph| {
                graph.child(
                    div().absolute().inset_0().flex().items_center().justify_center().child(
                        EmptyState::new(
                            IconName::Sparkles,
                            tr("No steps yet", "ステップはまだありません"),
                        )
                        .description(tr(
                            "Ask the commander for a plan, or add a step and write it yourself.",
                            "コマンダーに計画を頼むか、ステップを追加して自分で書いてください。",
                        ))
                        .action(
                            div()
                                .flex()
                                .gap(theme.spacing.sm)
                                .child(
                                    Button::new("add-first-node", tr("Add step", "ステップを追加"))
                                        .icon(IconName::Plus)
                                        .on_click(cx.listener(|this, _, _, cx| this.add_node(cx))),
                                )
                                .child(
                                    Button::new(
                                        "ask-commander",
                                        tr("Ask the commander", "コマンダーに頼む"),
                                    )
                                    .variant(ButtonVariant::Agent)
                                    .icon(IconName::Sparkles)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.close_workflow(cx)),
                                    ),
                                ),
                        ),
                    ),
                )
            })
            .child(self.legend(cx))
            .into_any_element()
    }
}
