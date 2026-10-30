//! Everything around the conversation: the sidebar, the workflow pane that opens beside it with
//! its canvas and inspector, and the screens shown before the daemon answers.

use atelier::agent::{Composer, ConversationRow};
use atelier::{
    ActiveTheme, Badge, Button, ButtonVariant, ControlSize, Edge, EmptyState, IconName, Label,
    ListItem, SectionHeader, Segment, SegmentedControl, Sidebar, Spinner, Switch, Tabs, TextColor,
    TextField, TextSize, Tone,
};
use gpui::{Context, IntoElement, ParentElement, Pixels, Styled, div, prelude::*, px};
use maestro_client::mirror::AgentView;
use maestro_core::{
    AgentId, Difficulty, ModelChoice, NodeEdit, NodeId, NodeOrigin, NodeStatus, ProviderKind,
};

use crate::app::{Maestro, Progress};
use crate::chat::{agent_status, conversation_title, provider_logo};
use crate::graph::{model_label, status_label, status_tone};
use crate::{input, link};

/// The inspector's width beside the canvas.
pub const INSPECTOR_WIDTH: f32 = 360.;

/// The inspector's height when it sits under the canvas.
const INSPECTOR_HEIGHT: f32 = 340.;

/// The providers a node can be pinned to, after "let the router choose".
const RUNS_ON: [&str; 5] = ["Auto", "Claude", "Codex", "Antigravity", "OpenCode"];

impl Maestro {
    fn caption(&self, label: &'static str, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        div()
            .px(theme.spacing.sm)
            .pt(theme.spacing.md)
            .pb(theme.spacing.xs)
            .child(Label::caption(label))
    }

    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let conversations: Vec<_> = self
            .mirror
            .commanders()
            .rev()
            .map(|(id, view)| {
                let status = agent_status(view.status, !view.transcript.entries.is_empty());
                let plans = self.mirror.plans.get(&id).map_or(0, Vec::len);
                let time = match plans {
                    0 => status.label().to_string(),
                    1 => "1 plan".into(),
                    plans => format!("{plans} plans"),
                };
                ConversationRow::new(
                    ("conversation", id.0 as usize),
                    conversation_title(view),
                    time,
                )
                .status(status)
                .selected(self.conversation == Some(id))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.conversation = Some(id);
                    this.workflow_full = false;
                    cx.notify();
                }))
                .into_any_element()
            })
            .collect();
        let workflows: Vec<_> = self
            .mirror
            .workflows
            .values()
            .rev()
            .map(|flow| {
                let id = flow.id;
                let total = flow.nodes().count();
                let done = flow.nodes().filter(|node| node.status == NodeStatus::Succeeded).count();
                let attention = flow.nodes().any(|node| {
                    matches!(node.status, NodeStatus::AwaitingUser | NodeStatus::Failed)
                });
                ListItem::new(("workflow", id.0 as usize), flow.title.clone())
                    .icon(if attention { IconName::Alert } else { IconName::Sliders })
                    .when(attention, |item| item.icon_color(theme.colors.warning))
                    .description(match total {
                        0 => "No steps".to_string(),
                        _ => format!("{done} of {total} steps done"),
                    })
                    .selected(self.workflow_open && self.workflow == Some(id))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_workflow(id);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let no_conversations = conversations.is_empty();
        let no_workflows = workflows.is_empty();
        Sidebar::new(Edge::Left)
            .w(px(crate::app::SIDEBAR_WIDTH))
            .p(theme.spacing.sm)
            .gap(theme.spacing.xxs)
            .child(
                Button::new("new-conversation", "New conversation")
                    .icon(IconName::Plus)
                    .full_width()
                    .on_click(cx.listener(|this, _, _, cx| this.new_conversation(cx))),
            )
            .child(
                div()
                    .id("sidebar-lists")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(theme.spacing.xxs)
                    .child(self.caption("Conversations", cx))
                    .children(conversations)
                    .when(no_conversations, |list| {
                        list.child(
                            div().px(theme.spacing.sm).child(
                                Label::new("Nothing yet")
                                    .size(TextSize::XSmall)
                                    .color(TextColor::Faint),
                            ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().flex_1().child(self.caption("Workflows", cx)))
                            .child(
                                Button::icon_only("new-workflow", IconName::Plus)
                                    .size(ControlSize::Small)
                                    .tooltip("New empty workflow")
                                    .on_click(cx.listener(|this, _, _, cx| this.new_workflow(cx))),
                            ),
                    )
                    .children(workflows)
                    .when(no_workflows, |list| {
                        list.child(
                            div().px(theme.spacing.sm).child(
                                Label::new("Plans the commander writes appear here")
                                    .size(TextSize::XSmall)
                                    .color(TextColor::Faint),
                            ),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.xxs)
                    .p(theme.spacing.sm)
                    .border_t_1()
                    .border_color(theme.colors.separator)
                    .child(Label::new("Repository").size(TextSize::XSmall).color(TextColor::Faint))
                    .child(
                        Label::new(match &self.project.repository {
                            Some(path) => path.display().to_string(),
                            None => "Not chosen".into(),
                        })
                        .size(TextSize::Small)
                        .mono()
                        .truncate(),
                    ),
            )
    }

    /// The repository and base branch fields.
    pub(crate) fn project_fields(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let place = link::describe(&self.launch);
        let field = |label: String, state| {
            div()
                .flex()
                .flex_col()
                .gap(theme.spacing.xs)
                .child(Label::new(label).size(TextSize::Small).color(TextColor::Muted))
                .child(TextField::new().child(input::editor(state)))
        };
        div()
            .flex()
            .flex_col()
            .gap(theme.spacing.md)
            .child(field(format!("Repository on {place}"), &self.fields.repository))
            .child(field("Base branch".into(), &self.fields.base))
    }

    pub(crate) fn connecting(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(theme.spacing.md)
            .bg(theme.colors.background)
            .child(Spinner::new("connecting").size(px(20.)).color(theme.colors.accent))
            .child(
                Label::new(format!("Connecting to {}…", link::describe(&self.launch)))
                    .color(TextColor::Muted),
            )
    }

    pub(crate) fn lost(&self, reason: String, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        div().flex().flex_1().items_center().justify_center().bg(theme.colors.background).child(
            EmptyState::new(IconName::Alert, "Can't reach the daemon").description(reason).action(
                Button::new("reconnect", "Try again").primary().icon(IconName::Refresh).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.connect(cx);
                        cx.notify();
                    }),
                ),
            ),
        )
    }

    /// The workflow on screen, opened beside the conversation or in its place (`full`): a bar
    /// with its commands, then the canvas and the inspector, side by side when the pane is wide
    /// enough and stacked when it is not. Beside the conversation the inspector waits for a
    /// selected step, since the conversation already shows the workflow as a whole.
    pub(crate) fn workflow_pane(
        &self,
        width: Pixels,
        full: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let Some(flow) = self.current() else { return div().into_any_element() };
        let workflow = flow.id;
        let progress = Progress::of(flow);
        let beside = width >= theme.density.split_width;
        let inspector_width = if beside { px(INSPECTOR_WIDTH) } else { width };
        let inspect = full || self.selected.is_some();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme.spacing.sm)
            .h(theme.density.toolbar)
            .px(theme.spacing.md)
            .border_b_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.chrome)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        Label::new(flow.title.clone())
                            .weight(theme.typography.label_weight)
                            .truncate(),
                    )
                    .child(
                        Label::new(format!("{} of {} steps done", progress.done, progress.total))
                            .size(TextSize::XSmall)
                            .color(TextColor::Muted)
                            .truncate(),
                    ),
            )
            .when(progress.waiting > 0, |bar| {
                bar.child(
                    Badge::new(format!("{} waiting for you", progress.waiting))
                        .tone(Tone::Warning)
                        .icon(IconName::Alert),
                )
            })
            .when(progress.failed > 0, |bar| {
                bar.child(
                    Badge::new(format!("{} failed", progress.failed))
                        .tone(Tone::Danger)
                        .icon(IconName::Alert),
                )
            })
            .child(if progress.finished {
                Button::new("publish", "Open pull request")
                    .primary()
                    .icon(IconName::ArrowUp)
                    .size(ControlSize::Small)
                    .on_click(cx.listener(move |this, _, _, cx| this.publish(workflow, cx)))
            } else {
                Button::new("run", "Run")
                    .primary()
                    .icon(IconName::Play)
                    .size(ControlSize::Small)
                    .disabled(!progress.ready)
                    .on_click(cx.listener(move |this, _, _, cx| this.run(workflow, cx)))
            })
            .when(self.chat_fits_beside(), |bar| {
                bar.child(
                    Button::icon_only(
                        "expand-workflow",
                        if self.workflow_full {
                            IconName::WindowRestore
                        } else {
                            IconName::WindowMaximize
                        },
                    )
                    .tooltip(if self.workflow_full {
                        "Show the conversation beside it"
                    } else {
                        "Fill the window"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workflow_full = !this.workflow_full;
                        cx.notify();
                    })),
                )
            })
            .child(
                Button::icon_only("close-workflow", IconName::Close)
                    .tooltip("Back to the conversation")
                    .on_click(cx.listener(|this, _, _, cx| this.close_workflow(cx))),
            );
        div()
            .flex()
            .flex_col()
            .w(width)
            .flex_none()
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(theme.colors.border)
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(!beside, |body| body.flex_col())
                    .child(self.graph(cx))
                    .when(inspect, |body| body.child(self.inspector(beside, inspector_width, cx))),
            )
            .into_any_element()
    }

    /// The selected node, its worker, or the workflow as a whole: a column on the trailing
    /// edge of the canvas when `beside`, otherwise a band under it.
    fn inspector(
        &self,
        beside: bool,
        width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let node = self.selected.and_then(|id| self.current()?.node(id)).cloned();
        let worker = node
            .as_ref()
            .and_then(|node| node.agent)
            .and_then(|agent| Some((agent, self.mirror.agents.get(&agent)?.clone())));
        let content = match &node {
            Some(node) if self.node_tab == 1 && worker.is_some() => {
                let (agent, view) = worker.clone().expect("checked above");
                self.worker(agent, &view, width, cx).into_any_element()
            }
            Some(node) => self.node_details(node, cx).into_any_element(),
            None => self.overview(cx).into_any_element(),
        };
        let tabs = worker.is_some().then(|| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .h(theme.density.toolbar)
                .px(theme.spacing.sm)
                .border_b_1()
                .border_color(theme.colors.border)
                .child(Tabs::new("node-tabs", ["Details", "Worker"], self.node_tab).on_select(
                    cx.listener(|this, index: &usize, _, cx| {
                        this.node_tab = *index;
                        cx.notify();
                    }),
                ))
        });
        if beside {
            Sidebar::new(Edge::Right).w(width).children(tabs).child(content).into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .flex_none()
                .w_full()
                .h(px(INSPECTOR_HEIGHT))
                .bg(theme.colors.surface)
                .border_t_1()
                .border_color(theme.colors.border)
                .children(tabs)
                .child(content)
                .into_any_element()
        }
    }

    fn node_details(
        &self,
        node: &maestro_core::Node,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let id = node.id;
        let flow = self.current();
        let dirty = input::text(&self.fields.title, cx) != node.title
            || input::text(&self.fields.instructions, cx) != node.instructions;
        let label =
            |text: &'static str| Label::new(text).size(TextSize::Small).color(TextColor::Muted);
        let group = || div().flex().flex_col().gap(theme.spacing.xs);
        let difficulty = node.difficulty.unwrap_or(Difficulty::Moderate);
        let runs_on = match &node.model {
            None => 0,
            Some(choice) => {
                1 + ProviderKind::ALL.iter().position(|kind| *kind == choice.provider).unwrap_or(0)
            }
        };
        let dependencies: Vec<(NodeId, String)> = flow
            .map(|flow| {
                flow.dependencies(id)
                    .filter_map(|dep| Some((dep, flow.node(dep)?.title.clone())))
                    .collect()
            })
            .unwrap_or_default();
        let rating = self.workflow.and_then(|workflow| self.ratings.get(&(workflow, id)).copied());
        let origin = match node.origin {
            NodeOrigin::Commander => "Planned by the commander",
            NodeOrigin::User => "Written by you",
        };

        div()
            .id("node-details")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap(theme.spacing.xl)
            .p(theme.spacing.lg)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.sm)
                    .child(SectionHeader::new(node.title.clone()).description(format!("{origin} · {}", node.id)))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(theme.spacing.sm)
                            .child(Badge::new(status_label(node.status)).tone(status_tone(node.status)).dot())
                            .children(node.branch.clone().map(|branch| Badge::new(branch).icon(IconName::Code))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.md)
                    .child(group().child(label("Title")).child(TextField::new().child(input::editor(&self.fields.title))))
                    .child(
                        group()
                            .child(label("Instructions"))
                            .child(
                                // A capsule suits one line; instructions run to paragraphs.
                                div()
                                    .w_full()
                                    .px(theme.spacing.sm)
                                    .py(theme.spacing.xs)
                                    .rounded(theme.shape.radius_md)
                                    .border_1()
                                    .border_color(theme.colors.border)
                                    .bg(theme.colors.sunken)
                                    .child(input::editor(&self.fields.instructions)),
                            ),
                    )
                    .child(
                        div().flex().child(
                            Button::new("save-node", "Save changes")
                                .primary()
                                .size(ControlSize::Small)
                                .disabled(!dirty)
                                .on_click(cx.listener(|this, _, _, cx| this.save_node(cx))),
                        ),
                    ),
            )
            .child(
                group()
                    .child(label("Difficulty"))
                    .child(
                        SegmentedControl::new(
                            "difficulty",
                            Difficulty::ALL.map(|difficulty| Segment::label(difficulty.label())),
                            Difficulty::ALL.iter().position(|d| *d == difficulty).unwrap_or(2),
                        )
                        .size(ControlSize::Small)
                        .on_select(cx.listener(move |this, index: &usize, _, cx| {
                            let edit = NodeEdit { difficulty: Some(Difficulty::ALL[*index]), ..NodeEdit::default() };
                            this.edit_node(id, edit, cx);
                        })),
                    )
                    .child(
                        Label::new("Decides the model's grade and effort when the router picks it")
                            .size(TextSize::XSmall)
                            .color(TextColor::Faint),
                    ),
            )
            .child(
                group()
                    .child(label("Runs on"))
                    .child(
                        // Five names do not fit a segmented control at the inspector's width.
                        div().flex().flex_wrap().gap(theme.spacing.xs).children(RUNS_ON.iter().enumerate().map(
                            |(index, name)| {
                                Button::new(("runs-on", index), *name)
                                    .size(ControlSize::Small)
                                    .selected(index == runs_on)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let model = (index > 0).then(|| ModelChoice {
                                            provider: ProviderKind::ALL[index - 1],
                                            model: String::new(),
                                            grade: difficulty.grade(),
                                            effort: difficulty.effort(),
                                        });
                                        this.edit_node(id, NodeEdit { model: Some(model), ..NodeEdit::default() }, cx);
                                    }))
                            },
                        )),
                    )
                    .child(Label::new(model_label(node)).size(TextSize::XSmall).color(TextColor::Faint)),
            )
            .child(
                group()
                    .child(label("Waits for"))
                    .when(dependencies.is_empty(), |list| {
                        list.child(
                            Label::new("Nothing; it can start first")
                                .size(TextSize::Small)
                                .color(TextColor::Faint),
                        )
                    })
                    .children(dependencies.into_iter().map(|(dep, title)| {
                        ListItem::new(("dependency", dep.0 as usize), title)
                            .icon(IconName::ChevronRight)
                            .trailing(
                                Button::icon_only(("drop-dependency", dep.0 as usize), IconName::Close)
                                    .size(ControlSize::Small)
                                    .tooltip("Stop waiting for this step")
                                    .on_click(cx.listener(move |this, _, _, cx| this.disconnect_nodes(dep, id, cx))),
                            )
                    })),
            )
            .when(node.status == NodeStatus::Succeeded, |details| {
                details.child(
                    group()
                        .child(label("How did it go?"))
                        .child(div().flex().gap(theme.spacing.xs).children((1..=5u8).map(|stars| {
                            Button::new(("rate", stars as usize), stars.to_string())
                                .size(ControlSize::Small)
                                .selected(rating == Some(stars))
                                .on_click(cx.listener(move |this, _, _, cx| this.rate(id, stars, cx)))
                        })))
                        .child(
                            Label::new("1 is poor, 5 excellent. Ratings teach the router which models to pick")
                                .size(TextSize::XSmall)
                                .color(TextColor::Faint),
                        ),
                )
            })
            .child(
                div().flex().child(
                    Button::new("delete-node", "Delete step")
                        .variant(ButtonVariant::Danger)
                        .icon(IconName::Trash)
                        .size(ControlSize::Small)
                        .disabled(matches!(node.status, NodeStatus::Running | NodeStatus::AwaitingUser))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_node(id, cx))),
                ),
            )
    }

    fn worker(
        &self,
        agent: AgentId,
        view: &AgentView,
        width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let rows = self.entries(agent, view, cx);
        let busy = view.transcript.busy;
        let provider = view.spec.model.provider;
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .id("worker-transcript")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(theme.spacing.lg)
                    .p(theme.spacing.lg)
                    .children(rows),
            )
            .child(
                div().flex_none().px(theme.spacing.sm).pb(theme.spacing.sm).child(input::stretch(
                    width - theme.spacing.sm * 2.,
                    Composer::new("worker-composer")
                        .input({
                            let this = cx.entity().downgrade();
                            input::message(&self.fields.node_message, move |window, cx| {
                                this.update(cx, |this, cx| this.send_to_node(window, cx)).ok();
                            })
                        })
                        .model(format!("{} · worker", provider.label()))
                        .when_some(provider_logo(provider), Composer::model_logo)
                        .busy(busy)
                        .on_send(cx.listener(|this, _, window, cx| this.send_to_node(window, cx)))
                        .on_stop(cx.listener(move |this, _, _, cx| this.interrupt(agent, cx))),
                )),
            )
    }

    /// The inspector with no node selected: the workflow as a whole.
    fn overview(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let Some(flow) = self.current() else { return div().into_any_element() };
        let total = flow.nodes().count();
        let done = flow.nodes().filter(|node| node.status == NodeStatus::Succeeded).count();
        let publish = flow.target.as_ref().is_some_and(|target| target.publish);
        let target = flow
            .target
            .as_ref()
            .map(|target| format!("{} from {}", target.repository.display(), target.base));
        let steps: Vec<_> = flow
            .topological_order()
            .into_iter()
            .filter_map(|id| flow.node(id))
            .map(|node| {
                let id = node.id;
                ListItem::new(("step", id.0 as usize), node.title.clone())
                    .description(model_label(node))
                    .trailing(
                        Badge::new(status_label(node.status)).tone(status_tone(node.status)).dot(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select(Some(id));
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .id("overview")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap(theme.spacing.xl)
            .p(theme.spacing.lg)
            .child(
                SectionHeader::new(flow.title.clone())
                    .description(format!("{done} of {total} steps done")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.md)
                    .child(Label::caption("Where it runs"))
                    .child(
                        Label::new(
                            target.unwrap_or_else(|| "Not set; Run uses the project below".into()),
                        )
                        .size(TextSize::Small)
                        .color(TextColor::Muted),
                    )
                    .child(self.project_fields(cx))
                    .child(
                        div().flex().child(
                            Button::new("use-project", "Use for this workflow")
                                .size(ControlSize::Small)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.save_project_target(cx)),
                                ),
                        ),
                    )
                    .child(
                        Switch::new("publish", publish)
                            .label("Open a pull request when every step is done")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_publish(!publish, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.xxs)
                    .child(Label::caption("Steps"))
                    .children(steps),
            )
            .into_any_element()
    }
}
