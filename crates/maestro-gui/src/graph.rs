//! The workflow canvas: nodes laid out in columns by dependency, wires from each node to the
//! nodes waiting for it, and the pointer gestures that edit the graph.
//!
//! Composed on a gpui `canvas` the way Atelier's node-editor example is. A workflow has one kind
//! of wire, "waits for", so every port shares the first port colour and the legend names it.
//! Where a node sits is this window's own business: the daemon's graph has no positions, so the
//! layout comes from [`Workflow::depths`] and a node the user drags keeps its place here.

use std::collections::HashMap;

use atelier::{
    ActiveTheme, Badge, Button, ButtonVariant, ControlSize, Divider, EmptyState, Icon, IconName,
    Label, ProgressBar, Spinner, StatusDot, TextColor, TextSize, Theme, Tone, Tooltip,
};
use gpui::{
    Bounds, Context, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, PathBuilder, Pixels, Point, ScrollWheelEvent, Styled, canvas, div, fill, point,
    prelude::*, px, size,
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
const ROW: f32 = 156.;
/// Where the graph starts before the user pans.
const ORIGIN: (f32, f32) = (40., 48.);
/// The pointer's target around a port.
const HIT: f32 = 18.;

/// What the pointer is doing on the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Drag {
    /// Nothing.
    #[default]
    None,
    /// Panning, from this pointer position.
    Pan(Point<Pixels>),
    /// Moving a node, from this pointer position.
    Node(NodeId, Point<Pixels>),
    /// A wire from a node's output, following the pointer (window coordinates).
    Wire {
        /// The node the dependents will wait for.
        from: NodeId,
        /// Where the pointer is.
        cursor: Point<Pixels>,
    },
}

/// The canvas's view state.
#[derive(Debug)]
pub struct Canvas {
    /// How far the graph is panned.
    pub pan: Point<Pixels>,
    /// Nodes the user moved, or that were added where the user could see them.
    placed: HashMap<(WorkflowId, NodeId), Point<Pixels>>,
    /// The gesture under way.
    pub drag: Drag,
}

impl Default for Canvas {
    fn default() -> Self {
        Self { pan: point(px(ORIGIN.0), px(ORIGIN.1)), placed: HashMap::new(), drag: Drag::None }
    }
}

impl Canvas {
    /// Back to the start of the graph.
    pub fn reset(&mut self) {
        self.pan = point(px(ORIGIN.0), px(ORIGIN.1));
        self.drag = Drag::None;
    }

    /// Puts a new node near the top-left of what is visible.
    pub fn place_new(&mut self, workflow: WorkflowId, node: NodeId) {
        let step = (node.0 % 6) as f32 * 24.;
        let at = point(px(120. + step), px(80. + step)) - self.pan;
        self.placed.insert((workflow, node), at);
    }

    /// Where every node of `flow` sits, before panning.
    pub fn layout(&self, flow: &Workflow) -> HashMap<NodeId, Point<Pixels>> {
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

    fn moved(&mut self, workflow: WorkflowId, node: NodeId, to: Point<Pixels>) {
        self.placed.insert((workflow, node), to);
    }
}

/// Where the ports sit on a node, so wires can be drawn without measuring the nodes.
struct Geometry {
    header: Pixels,
}

impl Geometry {
    fn new(theme: &Theme) -> Self {
        Self { header: theme.density.row + px(6.) }
    }

    /// The centre of a node's input (on the left) or output (on the right), before panning.
    /// The extra pixel is the node's border.
    fn port(&self, at: Point<Pixels>, output: bool) -> Point<Pixels> {
        let x = if output { at.x + px(NODE_WIDTH) } else { at.x };
        point(x, at.y + px(1.) + self.header / 2.)
    }
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
    // Pointer handling.

    fn canvas_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left {
            self.select(None);
        }
        self.canvas.drag = Drag::Pan(event.position);
        cx.notify();
    }

    fn canvas_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button.is_none() {
            // The button came up outside the window.
            if self.canvas.drag != Drag::None {
                self.canvas.drag = Drag::None;
                cx.notify();
            }
            return;
        }
        match self.canvas.drag {
            Drag::None => return,
            Drag::Pan(last) => {
                self.canvas.pan += event.position - last;
                self.canvas.drag = Drag::Pan(event.position);
            }
            Drag::Node(id, last) => {
                if let Some(flow) = self.current() {
                    let workflow = flow.id;
                    if let Some(at) = self.canvas.layout(flow).get(&id).copied() {
                        self.canvas.moved(workflow, id, at + (event.position - last));
                    }
                }
                self.canvas.drag = Drag::Node(id, event.position);
            }
            Drag::Wire { from, .. } => {
                self.canvas.drag = Drag::Wire { from, cursor: event.position };
            }
        }
        cx.notify();
    }

    fn canvas_mouse_up(&mut self, cx: &mut Context<Self>) {
        // A wire let go anywhere but on an input is dropped.
        self.canvas.drag = Drag::None;
        cx.notify();
    }

    fn canvas_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        self.canvas.pan += event.delta.pixel_delta(px(20.));
        cx.notify();
    }

    // Drawing.

    fn socket(
        &self,
        node: &Node,
        output: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let id = node.id;
        let color = theme.colors.ports[0];
        let flow = self.current();
        let connected = flow.is_some_and(|flow| {
            flow.edges().any(|edge| if output { edge.from == id } else { edge.to == id })
        });
        // While a wire is held, the node it comes from steps back: it cannot wait for itself.
        let compatible = match self.canvas.drag {
            Drag::Wire { from, .. } => output || from != id,
            _ => true,
        };
        let offset = -(px(1.) + px(HIT / 2.));
        let header = Geometry::new(theme).header;
        let tooltip = if output {
            tr("Steps that wait for this one", "このステップを待つステップ")
        } else {
            tr("Waits for", "待機先")
        };
        div()
            .id((if output { "out" } else { "in" }, id.0 as usize))
            .absolute()
            .top((header - px(HIT)) / 2.)
            .when(output, |hit| hit.right(offset))
            .when(!output, |hit| hit.left(offset))
            .size(px(HIT))
            .flex()
            .items_center()
            .justify_center()
            .cursor_crosshair()
            .opacity(if compatible { 1. } else { 0.3 })
            .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
            .child(
                div()
                    .size(px(10.))
                    .rounded_full()
                    .border_2()
                    .border_color(color)
                    .bg(if connected { color } else { theme.colors.elevated }),
            )
            .when(output, |hit| {
                hit.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.canvas.drag = Drag::Wire { from: id, cursor: event.position };
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
            })
            .when(!output, |hit| {
                hit.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                        if let Drag::Wire { from, .. } = this.canvas.drag {
                            this.connect_nodes(from, id, cx);
                        }
                        this.canvas.drag = Drag::None;
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
            })
    }

    fn state_mark(node: &Node, theme: &Theme) -> impl IntoElement + use<> {
        let mark = div().flex().flex_none().items_center().gap(theme.spacing.xs);
        let word = |text: &'static str, color: TextColor| {
            Label::new(text).size(TextSize::XSmall).color(color)
        };
        match node.status {
            NodeStatus::Pending if node.origin == NodeOrigin::Commander => {
                mark.child(Icon::new(IconName::Sparkles).size(px(12.)).color(theme.colors.agent))
            }
            NodeStatus::Pending => mark,
            NodeStatus::Running => {
                mark.child(word(status_label(node.status), TextColor::Muted)).child(
                    Spinner::new(("node-spinner", node.id.0 as usize))
                        .size(px(12.))
                        .color(theme.colors.accent),
                )
            }
            NodeStatus::AwaitingUser => mark
                .child(word(status_label(node.status), TextColor::Warning))
                .child(Icon::new(IconName::Alert).size(px(12.)).color(theme.colors.warning)),
            NodeStatus::AwaitingChoice => mark
                .child(word(status_label(node.status), TextColor::Warning))
                .child(Icon::new(IconName::Eye).size(px(12.)).color(theme.colors.warning)),
            NodeStatus::Succeeded => mark
                .child(word(status_label(node.status), TextColor::Faint))
                .child(Icon::new(IconName::Check).size(px(12.)).color(theme.colors.success)),
            NodeStatus::Failed => mark
                .child(word(status_label(node.status), TextColor::Danger))
                .child(Icon::new(IconName::Alert).size(px(12.)).color(theme.colors.danger)),
        }
    }

    fn render_node(
        &self,
        node: &Node,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let geometry = Geometry::new(&theme);
        let id = node.id;
        let selected = self.selected == Some(id);
        let dragging = matches!(self.canvas.drag, Drag::Node(dragged, _) if dragged == id);
        // A step the commander planned is the AI's draft until the user keeps it or it runs.
        let draft = node.origin == NodeOrigin::Commander && node.status == NodeStatus::Pending;
        let border = match node.status {
            _ if draft => theme.colors.agent,
            NodeStatus::AwaitingUser | NodeStatus::AwaitingChoice => theme.colors.warning,
            NodeStatus::Failed => theme.colors.danger,
            NodeStatus::Running => theme.colors.accent,
            _ if selected => theme.colors.accent,
            _ => theme.colors.border_strong,
        };
        let inner = Theme::concentric(theme.shape.radius_md, px(1.));

        let header = div()
            .relative()
            .h(geometry.header)
            .flex()
            .items_center()
            .gap(theme.spacing.sm)
            .px(theme.spacing.md)
            .rounded_t(inner)
            .border_b_1()
            .border_color(theme.colors.separator)
            .when(selected, |header| header.bg(theme.colors.selected))
            .child(
                div().flex_1().min_w_0().child(
                    Label::new(node.title.clone())
                        .size(TextSize::Small)
                        .weight(theme.typography.label_weight)
                        .truncate(),
                ),
            )
            .child(Self::state_mark(node, &theme))
            .child(self.socket(node, false, cx))
            .child(self.socket(node, true, cx));

        let body = div()
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
                    .children(
                        node.difficulty.map(|difficulty| Badge::new(i18n::difficulty(difficulty))),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Label::new(model_label(node))
                                .size(TextSize::XSmall)
                                .color(TextColor::Muted)
                                .truncate(),
                        ),
                    ),
            )
            .child(
                Label::new(if node.instructions.is_empty() {
                    tr("No instructions yet", "指示はまだありません").to_string()
                } else {
                    node.instructions.lines().next().unwrap_or_default().to_string()
                })
                .size(TextSize::XSmall)
                .color(TextColor::Faint)
                .truncate(),
            )
            .children(node.branch.clone().map(|branch| {
                Label::new(branch).size(TextSize::XSmall).mono().color(TextColor::Faint).truncate()
            }));

        div()
            .id(("node", id.0 as usize))
            .absolute()
            .left(self.canvas.pan.x + at.x)
            .top(self.canvas.pan.y + at.y)
            .w(px(NODE_WIDTH))
            .flex()
            .flex_col()
            .rounded(theme.shape.radius_md)
            .bg(theme.colors.elevated)
            .border_1()
            .border_color(border)
            .when(draft, |node| node.border_dashed())
            .shadow(if dragging {
                theme.elevation.medium.clone()
            } else {
                theme.elevation.low.clone()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.select(Some(id));
                    this.canvas.drag = Drag::Node(id, event.position);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(header)
            .when(node.status == NodeStatus::Running, |node_box| {
                node_box.child(
                    ProgressBar::indeterminate(("node-progress", id.0 as usize)).height(px(2.)),
                )
            })
            .child(body)
            .when(draft, |node_box| {
                node_box.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme.spacing.sm)
                        .px(theme.spacing.md)
                        .py(theme.spacing.sm)
                        .rounded_b(inner)
                        .border_t_1()
                        .border_color(theme.colors.separator)
                        .bg(theme.colors.agent_wash)
                        .child(
                            div().flex_1().min_w_0().child(
                                Label::new(tr("Draft", "下書き"))
                                    .size(TextSize::XSmall)
                                    .color(TextColor::Agent)
                                    .truncate(),
                            ),
                        )
                        .child(
                            Button::new(("undo-node", id.0 as usize), tr("Remove", "削除"))
                                .ghost()
                                .size(ControlSize::Small)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.remove_node(id, cx)),
                                ),
                        )
                        .child(
                            Button::new(("keep-node", id.0 as usize), tr("Keep", "採用"))
                                .size(ControlSize::Small)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.edit_node(id, NodeEdit::default(), cx);
                                })),
                        ),
                )
            })
    }

    /// The dot grid and the wires, painted under the nodes.
    fn wires(
        &self,
        flow: &Workflow,
        layout: &HashMap<NodeId, Point<Pixels>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let geometry = Geometry::new(theme);
        let pan = self.canvas.pan;
        let color = theme.colors.ports[0];
        let wires: Vec<_> = flow
            .edges()
            .filter_map(|edge| {
                let strong = self.selected == Some(edge.from) || self.selected == Some(edge.to);
                Some((
                    pan + geometry.port(*layout.get(&edge.from)?, true),
                    pan + geometry.port(*layout.get(&edge.to)?, false),
                    strong,
                ))
            })
            .collect();
        let pending = match self.canvas.drag {
            Drag::Wire { from, cursor } => {
                layout.get(&from).map(|at| (pan + geometry.port(*at, true), cursor))
            }
            _ => None,
        };
        let dot = theme.colors.grid;
        let spacing = px(24.);
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window, _| {
                let origin = bounds.origin;
                let mut y = pan.y % spacing;
                while y < bounds.size.height {
                    let mut x = pan.x % spacing;
                    while x < bounds.size.width {
                        window.paint_quad(fill(
                            Bounds::new(origin + point(x, y), size(px(1.5), px(1.5))),
                            dot,
                        ));
                        x += spacing;
                    }
                    y += spacing;
                }
                for (from, to, strong) in &wires {
                    let width = if *strong { px(3.) } else { px(2.) };
                    if let Some(path) = bezier(origin + *from, origin + *to, width) {
                        window.paint_path(path, if *strong { color } else { color.opacity(0.75) });
                    }
                }
                if let Some((from, cursor)) = pending
                    && let Some(path) = bezier(origin + from, cursor, px(2.))
                {
                    window.paint_path(path, color);
                }
            },
        )
        .size_full()
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
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.canvas.reset();
                        cx.notify();
                    })),
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
                            "Waits for · drag from a step's right edge to another step",
                            "待機関係 · ステップの右端から別のステップへドラッグ",
                        ))
                        .size(TextSize::XSmall)
                        .color(TextColor::Muted),
                    ),
            )
    }

    /// The canvas for the workflow on screen.
    pub(crate) fn graph(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let Some(flow) = self.current().cloned() else {
            return div().flex_1().into_any_element();
        };
        let layout = self.canvas.layout(&flow);
        // The selected node is drawn last, so it is on top while it is dragged.
        let mut order: Vec<_> = flow.nodes().collect();
        order.sort_by_key(|node| self.selected == Some(node.id));
        let nodes: Vec<_> = order
            .into_iter()
            .filter_map(|node| {
                let at = *layout.get(&node.id)?;
                Some(self.render_node(node, at, cx).into_any_element())
            })
            .collect();
        let empty = nodes.is_empty();
        div()
            .id("graph")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(theme.colors.background)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, _, cx| this.canvas_mouse_down(event, cx)),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event, _, cx| this.canvas_mouse_down(event, cx)),
            )
            .on_mouse_move(cx.listener(|this, event, _, cx| this.canvas_mouse_move(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.canvas_mouse_up(cx)))
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _, _, cx| this.canvas_mouse_up(cx)),
            )
            .on_scroll_wheel(cx.listener(|this, event, _, cx| this.canvas_scroll(event, cx)))
            .child(div().absolute().inset_0().child(self.wires(&flow, &layout, cx)))
            .children(nodes)
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

/// A wire from an output to an input, leaving and entering horizontally.
fn bezier(from: Point<Pixels>, to: Point<Pixels>, width: Pixels) -> Option<gpui::Path<Pixels>> {
    let reach = ((to.x - from.x).abs() * 0.5).max(px(48.));
    let mut builder = PathBuilder::stroke(width);
    builder.move_to(from);
    builder.cubic_bezier_to(to, point(from.x + reach, from.y), point(to.x - reach, to.y));
    builder.build().ok()
}
