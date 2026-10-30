//! Everything around the canvas: the sidebar, the commander's panel, the inspector, and the
//! screens shown before there is a workflow to draw.

use atelier::agent::{
    AgentPanel, ApprovalCard, AssistantMessage, ChatNotice, Composer, ConversationRow,
    ReasoningBlock, ThinkingIndicator, ToolCall, ToolGroup, UserMessage,
};
use atelier::{
    ActiveTheme, Badge, Button, ButtonVariant, Card, ControlSize, Edge, EmptyState, IconName,
    Label, ListItem, SectionHeader, Segment, SegmentedControl, Sidebar, Spinner, Switch, Tabs,
    TextColor, TextField, TextSize,
};
use gpui::{
    AnyElement, Context, ElementId, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use maestro_client::mirror::{AgentView, Entry, ToolState};
use maestro_core::{
    AgentId, Difficulty, ModelChoice, NodeEdit, NodeId, NodeOrigin, NodeStatus, PermissionKind,
    ProviderKind,
};
use maestro_protocol::AgentStatus;

use crate::app::Maestro;
use crate::graph::{model_label, status_label, status_tone};
use crate::i18n::{self, tr};
use crate::{input, link, trf};

/// How an agent's status reads in Atelier's vocabulary.
pub fn agent_status(status: AgentStatus, has_history: bool) -> atelier::agent::AgentStatus {
    use atelier::agent::AgentStatus as Shown;
    match status {
        AgentStatus::Working => Shown::Running,
        AgentStatus::AwaitingPermission => Shown::AwaitingApproval,
        AgentStatus::Idle if has_history => Shown::Completed,
        AgentStatus::Idle => Shown::Idle,
        AgentStatus::Exited => Shown::Failed,
    }
}

/// A conversation's name: the start of its first message.
fn conversation_title(view: &AgentView) -> String {
    let first = view.transcript.entries.iter().find_map(|entry| match entry {
        Entry::User(text) => Some(text.as_str()),
        _ => None,
    });
    let Some(first) = first else { return tr("New conversation", "新しい会話").into() };
    let line = first.lines().next().unwrap_or_default();
    match line.char_indices().nth(48) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.into(),
    }
}

/// An element id for a transcript row.
fn element(kind: &str, key: &str) -> ElementId {
    ElementId::Name(format!("{kind}-{key}").into())
}

/// A reply with its plan blocks folded away: the plan is on the canvas, and the panel offers
/// to open it.
fn without_plans(text: &str) -> String {
    let opening = format!("```{}", maestro_core::plan::PLAN_FENCE);
    let mut shown = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(&opening) {
        shown.push_str(&rest[..start]);
        let body = &rest[start + opening.len()..];
        let Some(end) = body.find("```") else {
            // Still streaming: hide the half-written plan too.
            rest = "";
            break;
        };
        shown.push_str(tr("(The plan is in the workflow.)", "（計画はワークフローにあります）"));
        rest = &body[end + 3..];
    }
    shown.push_str(rest);
    shown
}

/// What allowing a request of this kind lets the agent do.
fn approval_body(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Command => tr(
            "The agent wants to run this command in its worktree.",
            "エージェントが自分のワークツリーでこのコマンドを実行しようとしています。",
        ),
        PermissionKind::FileChange => tr(
            "The agent wants to change files in its worktree.",
            "エージェントが自分のワークツリーのファイルを変更しようとしています。",
        ),
        PermissionKind::Other => tr(
            "The agent wants to do something its permissions do not cover.",
            "エージェントが権限の範囲外の操作をしようとしています。",
        ),
    }
}

/// The inspector's width; the commander's panel uses its own.
const INSPECTOR_WIDTH: f32 = 360.;

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
                    0 => i18n::agent_status(status).to_string(),
                    1 => tr("1 plan", "計画 1 件").into(),
                    plans => trf!("{plans} plans", "計画 {plans} 件"),
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
                    this.panel_open = true;
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
                        0 => tr("No steps", "ステップなし").to_string(),
                        _ => trf!("{done} of {total} steps done", "{total} ステップ中 {done} 完了"),
                    })
                    .selected(self.workflow == Some(id))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.show_workflow(id);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let no_conversations = conversations.is_empty();
        let no_workflows = workflows.is_empty();
        Sidebar::new(Edge::Left)
            .w(px(256.))
            .p(theme.spacing.sm)
            .gap(theme.spacing.xxs)
            .child(
                Button::new("new-conversation", tr("New conversation", "新しい会話"))
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
                    .child(self.caption(tr("Conversations", "会話"), cx))
                    .children(conversations)
                    .when(no_conversations, |list| {
                        list.child(
                            div().px(theme.spacing.sm).child(
                                Label::new(tr("Nothing yet", "まだありません"))
                                    .size(TextSize::XSmall)
                                    .color(TextColor::Faint),
                            ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .child(self.caption(tr("Workflows", "ワークフロー"), cx)),
                            )
                            .child(
                                Button::icon_only("new-workflow", IconName::Plus)
                                    .size(ControlSize::Small)
                                    .tooltip(tr("New empty workflow", "空のワークフローを作成"))
                                    .on_click(cx.listener(|this, _, _, cx| this.new_workflow(cx))),
                            ),
                    )
                    .children(workflows)
                    .when(no_workflows, |list| {
                        list.child(
                            div().px(theme.spacing.sm).child(
                                Label::new(tr(
                                    "Plans the commander writes appear here",
                                    "コマンダーが書いた計画がここに表示されます",
                                ))
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
                    .child(
                        Label::new(tr("Repository", "リポジトリ"))
                            .size(TextSize::XSmall)
                            .color(TextColor::Faint),
                    )
                    .child(
                        Label::new(match &self.project.repository {
                            Some(path) => path.display().to_string(),
                            None => tr("Not chosen", "未選択").into(),
                        })
                        .size(TextSize::Small)
                        .mono()
                        .truncate(),
                    ),
            )
    }

    /// The repository and base branch fields.
    fn project_fields(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
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
            .child(field(
                trf!("Repository on {place}", "{place}上のリポジトリ"),
                &self.fields.repository,
            ))
            .child(field(tr("Base branch", "ベースブランチ").into(), &self.fields.base))
    }

    /// What the canvas shows before there is a workflow.
    pub(crate) fn welcome(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        div()
            .id("welcome")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_center()
            .overflow_y_scroll()
            .bg(theme.colors.background)
            .p(theme.spacing.xxl)
            .child(
                Card::new()
                    .w_full()
                    .max_w(px(560.))
                    .p(theme.spacing.xl)
                    .gap(theme.spacing.xl)
                    .bg(theme.colors.surface)
                    .child(SectionHeader::new(tr("Plan with the commander", "コマンダーと計画する")).description(
                        tr(
                            "Tell the commander what you want built. It plans the work as a \
                             graph of steps; each step gets its own worker and its own git \
                             worktree, and you can edit the graph before it runs.",
                            "作りたいものをコマンダーに伝えてください。コマンダーは作業をステップの\
                             グラフとして計画し、各ステップには専用のワーカーと git ワークツリーが\
                             割り当てられます。実行前にグラフを編集することもできます。",
                        ),
                    ))
                    .child(self.project_fields(cx))
                    .child(
                        div()
                            .flex()
                            .gap(theme.spacing.sm)
                            .child(
                                Button::new("welcome-ask", tr("Ask the commander", "コマンダーに頼む"))
                                    .primary()
                                    .icon(IconName::Sparkles)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.new_conversation(cx)),
                                    ),
                            )
                            .child(
                                Button::new(
                                    "welcome-empty",
                                    tr("Start from an empty workflow", "空のワークフローから始める"),
                                )
                                    .on_click(cx.listener(|this, _, _, cx| this.new_workflow(cx))),
                            ),
                    ),
            )
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
                Label::new(trf!("Connecting to {}…", "{}に接続中…", link::describe(&self.launch)))
                    .color(TextColor::Muted),
            )
    }

    pub(crate) fn lost(&self, reason: String, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        div().flex().flex_1().items_center().justify_center().bg(theme.colors.background).child(
            EmptyState::new(
                IconName::Alert,
                tr("Can't reach the daemon", "デーモンに接続できません"),
            )
            .description(reason)
            .action(
                Button::new("reconnect", tr("Try again", "再試行"))
                    .primary()
                    .icon(IconName::Refresh)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.connect(cx);
                        cx.notify();
                    })),
            ),
        )
    }

    /// An agent's conversation as transcript rows.
    fn entries(&self, agent: AgentId, view: &AgentView, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        let meta = match &view.spec.model.model {
            model if model.is_empty() => view.spec.model.provider.label().to_string(),
            model => format!("{} · {model}", view.spec.model.provider.label()),
        };
        for (index, entry) in view.transcript.entries.iter().enumerate() {
            let key: SharedString = format!("{agent}-{index}").into();
            let open = self.expanded.contains(&key);
            let toggle = {
                let key = key.clone();
                cx.listener(move |this, _, _, cx| {
                    if !this.expanded.remove(&key) {
                        this.expanded.insert(key.clone());
                    }
                    cx.notify();
                })
            };
            let row = match entry {
                Entry::User(text) => {
                    UserMessage::new(text.clone()).id(element("user", &key)).into_any_element()
                }
                Entry::Assistant { text, done } => AssistantMessage::new(without_plans(text))
                    .id(element("assistant", &key))
                    .meta(meta.clone())
                    .streaming(!done)
                    .into_any_element(),
                Entry::Reasoning(text) => ReasoningBlock::new(
                    element("reasoning", &key),
                    tr("Reasoning", "推論"),
                    text.clone(),
                )
                .open(open)
                .on_toggle(toggle)
                .into_any_element(),
                Entry::Tools(tools) => {
                    let title = match tools.len() {
                        1 => tr("Used 1 tool", "ツールを 1 回使用").to_string(),
                        count => trf!("Used {count} tools", "ツールを {count} 回使用"),
                    };
                    let mut group = ToolGroup::new(title);
                    for (call, tool) in tools.iter().enumerate() {
                        let call_key: SharedString = format!("{key}-{call}").into();
                        let open = self.expanded.contains(&call_key);
                        let state = match tool.state {
                            ToolState::Running => atelier::agent::ToolState::Running,
                            ToolState::Succeeded => atelier::agent::ToolState::Succeeded,
                            ToolState::Failed => atelier::agent::ToolState::Failed,
                        };
                        let summary: String = tool.call.input.chars().take(80).collect();
                        let detail = if tool.output.is_empty() {
                            tool.call.input.clone()
                        } else {
                            format!("{}\n\n{}", tool.call.input, tool.output)
                        };
                        group = group.call(
                            ToolCall::new(
                                element("tool", &call_key),
                                tool.call.name.clone(),
                                summary,
                                state,
                            )
                            .icon(IconName::Wrench)
                            .detail(detail)
                            .open(open)
                            .on_toggle(cx.listener(
                                move |this, _, _, cx| {
                                    if !this.expanded.remove(&call_key) {
                                        this.expanded.insert(call_key.clone());
                                    }
                                    cx.notify();
                                },
                            )),
                        );
                    }
                    group.into_any_element()
                }
                Entry::Permission { request, open: true, .. } => {
                    let (allow, deny) = (request.id.clone(), request.id.clone());
                    ApprovalCard::new(
                        element("approval", &key),
                        trf!("Allow: {}?", "許可しますか: {}", request.summary),
                    )
                    .body(approval_body(request.kind))
                    .allow_label(tr("Allow", "許可"))
                    .on_allow(cx.listener(move |this, _, _, cx| {
                        this.answer(agent, allow.clone(), true, cx);
                    }))
                    .on_deny(cx.listener(move |this, _, _, cx| {
                        this.answer(agent, deny.clone(), false, cx);
                    }))
                    .into_any_element()
                }
                Entry::Permission { request, allowed, .. } => ChatNotice::status(match allowed {
                    Some(true) => trf!("Allowed: {}", "許可済み: {}", request.summary),
                    _ => trf!("Refused: {}", "拒否: {}", request.summary),
                })
                .into_any_element(),
                Entry::Failed(message) => ChatNotice::error(trf!(
                    "The turn failed: {message}",
                    "ターンが失敗しました: {message}"
                ))
                .into_any_element(),
                Entry::Exited(reason) => ChatNotice::status(match reason {
                    Some(reason) => {
                        trf!("The agent stopped: {reason}", "エージェントが停止しました: {reason}")
                    }
                    None => tr("The agent stopped", "エージェントが停止しました").into(),
                })
                .into_any_element(),
            };
            rows.push(row);
        }
        if view.transcript.busy && matches!(view.transcript.entries.last(), Some(Entry::User(_))) {
            rows.push(ThinkingIndicator::new(tr("Thinking", "考え中")).into_any_element());
        }
        rows
    }

    pub(crate) fn commander_panel(
        &self,
        width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let view =
            self.conversation.and_then(|agent| Some((agent, self.mirror.agents.get(&agent)?)));
        let (title, status, provider) = match view {
            Some((_, view)) => (
                conversation_title(view),
                agent_status(view.status, !view.transcript.entries.is_empty()),
                view.spec.model.provider,
            ),
            None => (
                tr("New conversation", "新しい会話").into(),
                atelier::agent::AgentStatus::Idle,
                self.provider,
            ),
        };
        let busy = view.is_some_and(|(_, view)| view.transcript.busy);
        let fresh = view.is_none();
        let mut panel = AgentPanel::new("commander", title)
            .status(status)
            .status_detail(trf!("Commander · {}", "コマンダー · {}", provider.label()))
            .action(
                Button::icon_only("panel-new-conversation", IconName::Plus)
                    .tooltip(tr("New conversation", "新しい会話"))
                    .on_click(cx.listener(|this, _, _, cx| this.new_conversation(cx))),
            )
            .on_close(cx.listener(|this, _, _, cx| {
                this.panel_open = false;
                cx.notify();
            }));
        if let Some((agent, view)) = view {
            for row in self.entries(agent, view, cx) {
                panel = panel.entry(row);
            }
            let plans = self.mirror.plans.get(&agent).cloned().unwrap_or_default();
            for workflow in plans {
                let Some(flow) = self.mirror.workflows.get(&workflow) else { continue };
                panel = panel.entry(
                    div().flex().child(
                        Button::new(
                            ("open-plan", workflow.0 as usize),
                            trf!("Open \u{201c}{}\u{201d}", "\u{300c}{}\u{300d}を開く", flow.title),
                        )
                        .variant(ButtonVariant::Agent)
                        .icon(IconName::Sparkles)
                        .size(ControlSize::Small)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.show_workflow(workflow);
                            cx.notify();
                        })),
                    ),
                );
            }
        }
        panel = panel.empty_state(
            div()
                .flex()
                .flex_col()
                .gap(theme.spacing.md)
                .child(SectionHeader::new(tr("What should be built?", "何を作りますか？")).description(tr(
                    "The commander reads the repository, talks the plan through with you, and \
                     writes it as a workflow. It never changes files itself.",
                    "コマンダーはリポジトリを読み、あなたと計画を話し合ってワークフローとして\
                     書き出します。ファイルを自分で変更することはありません。",
                )))
                .when(self.project.repository.is_none(), |empty| {
                    empty.child(self.project_fields(cx))
                }),
        );
        let composer = Composer::new("commander-composer")
            .input({
                let this = cx.entity().downgrade();
                input::message(&self.fields.composer, move |window, cx| {
                    this.update(cx, |this, cx| this.send_to_commander(window, cx)).ok();
                })
            })
            .model(provider.label())
            .busy(busy)
            .on_send(cx.listener(|this, _, window, cx| this.send_to_commander(window, cx)))
            .on_stop(cx.listener(|this, _, _, cx| this.stop_commander(cx)))
            .when(fresh, |composer| {
                // The provider is fixed once the conversation starts.
                composer.on_model(cx.listener(|this, _, _, cx| {
                    let index = ProviderKind::ALL.iter().position(|kind| *kind == this.provider);
                    this.provider =
                        ProviderKind::ALL[(index.unwrap_or(0) + 1) % ProviderKind::ALL.len()];
                    cx.notify();
                }))
            });
        div()
            .w(px(width))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(theme.colors.border)
            .child(
                panel
                    .composer(input::stretch(px(width) - theme.spacing.sm * 2. - px(1.), composer)),
            )
    }

    pub(crate) fn inspector(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let node = self.selected.and_then(|id| self.current()?.node(id)).cloned();
        let worker = node
            .as_ref()
            .and_then(|node| node.agent)
            .and_then(|agent| Some((agent, self.mirror.agents.get(&agent)?.clone())));
        let content = match &node {
            Some(node) if self.node_tab == 1 && worker.is_some() => {
                let (agent, view) = worker.clone().expect("checked above");
                self.worker(node.id, agent, &view, cx).into_any_element()
            }
            Some(node) => self.node_details(node, cx).into_any_element(),
            None => self.overview(cx).into_any_element(),
        };
        Sidebar::new(Edge::Right)
            .w(px(INSPECTOR_WIDTH))
            .when(worker.is_some(), |side| {
                side.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .h(theme.density.toolbar)
                        .px(theme.spacing.sm)
                        .border_b_1()
                        .border_color(theme.colors.border)
                        .child(
                            Tabs::new(
                                "node-tabs",
                                [tr("Details", "詳細"), tr("Worker", "ワーカー")],
                                self.node_tab,
                            )
                            .on_select(cx.listener(
                                |this, index: &usize, _, cx| {
                                    this.node_tab = *index;
                                    cx.notify();
                                },
                            )),
                        ),
                )
            })
            .child(content)
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
            NodeOrigin::Commander => tr("Planned by the commander", "コマンダーが計画"),
            NodeOrigin::User => tr("Written by you", "あなたが作成"),
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
                    .child(group().child(label(tr("Title", "タイトル"))).child(TextField::new().child(input::editor(&self.fields.title))))
                    .child(
                        group()
                            .child(label(tr("Instructions", "指示")))
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
                            Button::new("save-node", tr("Save changes", "変更を保存"))
                                .primary()
                                .size(ControlSize::Small)
                                .disabled(!dirty)
                                .on_click(cx.listener(|this, _, _, cx| this.save_node(cx))),
                        ),
                    ),
            )
            .child(
                group()
                    .child(label(tr("Difficulty", "難易度")))
                    .child(
                        SegmentedControl::new(
                            "difficulty",
                            Difficulty::ALL.map(|difficulty| Segment::label(i18n::difficulty(difficulty))),
                            Difficulty::ALL.iter().position(|d| *d == difficulty).unwrap_or(2),
                        )
                        .size(ControlSize::Small)
                        .on_select(cx.listener(move |this, index: &usize, _, cx| {
                            let edit = NodeEdit { difficulty: Some(Difficulty::ALL[*index]), ..NodeEdit::default() };
                            this.edit_node(id, edit, cx);
                        })),
                    )
                    .child(
                        Label::new(tr(
                            "Decides the model's grade and effort when the router picks it",
                            "ルーターが選ぶときのモデルのグレードとエフォートを決めます",
                        ))
                            .size(TextSize::XSmall)
                            .color(TextColor::Faint),
                    ),
            )
            .child(
                group()
                    .child(label(tr("Runs on", "実行先")))
                    .child(
                        // Five names do not fit a segmented control at the inspector's width.
                        div().flex().flex_wrap().gap(theme.spacing.xs).children(RUNS_ON.iter().enumerate().map(
                            |(index, name)| {
                                Button::new(("runs-on", index), if index == 0 { tr("Auto", "自動") } else { name })
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
                    .child(label(tr("Waits for", "待機先")))
                    .when(dependencies.is_empty(), |list| {
                        list.child(
                            Label::new(tr("Nothing; it can start first", "なし。最初に開始できます"))
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
                                    .tooltip(tr("Stop waiting for this step", "このステップを待たない"))
                                    .on_click(cx.listener(move |this, _, _, cx| this.disconnect_nodes(dep, id, cx))),
                            )
                    })),
            )
            .when(node.status == NodeStatus::Succeeded, |details| {
                details.child(
                    group()
                        .child(label(tr("How did it go?", "結果はどうでしたか？")))
                        .child(div().flex().gap(theme.spacing.xs).children((1..=5u8).map(|stars| {
                            Button::new(("rate", stars as usize), stars.to_string())
                                .size(ControlSize::Small)
                                .selected(rating == Some(stars))
                                .on_click(cx.listener(move |this, _, _, cx| this.rate(id, stars, cx)))
                        })))
                        .child(
                            Label::new(tr(
                                "1 is poor, 5 excellent. Ratings teach the router which models to pick",
                                "1 が最低、5 が最高。評価はルーターのモデル選びに使われます",
                            ))
                                .size(TextSize::XSmall)
                                .color(TextColor::Faint),
                        ),
                )
            })
            .child(
                div().flex().child(
                    Button::new("delete-node", tr("Delete step", "ステップを削除"))
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
        node: NodeId,
        agent: AgentId,
        view: &AgentView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let rows = self.entries(agent, view, cx);
        let busy = view.transcript.busy;
        let _ = node;
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
                    px(INSPECTOR_WIDTH) - theme.spacing.sm * 2.,
                    Composer::new("worker-composer")
                        .input({
                            let this = cx.entity().downgrade();
                            input::message(&self.fields.node_message, move |window, cx| {
                                this.update(cx, |this, cx| this.send_to_node(window, cx)).ok();
                            })
                        })
                        .model(trf!(
                            "{} · worker",
                            "{} · ワーカー",
                            view.spec.model.provider.label()
                        ))
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
        let target = flow.target.as_ref().map(|target| {
            trf!("{} from {}", "{}（{} から）", target.repository.display(), target.base)
        });
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
            .child(SectionHeader::new(flow.title.clone()).description(trf!(
                "{done} of {total} steps done",
                "{total} ステップ中 {done} 完了"
            )))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.md)
                    .child(Label::caption(tr("Where it runs", "実行場所")))
                    .child(
                        Label::new(target.unwrap_or_else(|| {
                            tr(
                                "Not set; Run uses the project below",
                                "未設定。実行時は下のプロジェクトを使います",
                            )
                            .into()
                        }))
                        .size(TextSize::Small)
                        .color(TextColor::Muted),
                    )
                    .child(self.project_fields(cx))
                    .child(
                        div().flex().child(
                            Button::new(
                                "use-project",
                                tr("Use for this workflow", "このワークフローに使う"),
                            )
                            .size(ControlSize::Small)
                            .on_click(cx.listener(|this, _, _, cx| this.save_project_target(cx))),
                        ),
                    )
                    .child(
                        Switch::new("publish", publish)
                            .label(tr(
                                "Open a pull request when every step is done",
                                "全ステップ完了時にプルリクエストを作成",
                            ))
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
                    .child(Label::caption(tr("Steps", "ステップ")))
                    .children(steps),
            )
            .into_any_element()
    }
}
