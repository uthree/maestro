//! The conversation with the commander, which fills the middle of the window. The workflow it
//! plans is shown in the chat as a card and opens beside it on demand.

use atelier::agent::{
    ApprovalCard, AssistantMessage, ChatNotice, Composer, ReasoningBlock, ThinkingIndicator,
    ToolCall, ToolGroup, UserMessage,
};
use atelier::{
    ActiveTheme, Badge, Button, ButtonVariant, Card, ControlSize, CopyButton, IconName, Label,
    Popover, ProviderLogo, SectionHeader, StepList, StepState, TextColor, TextSize, Tone,
};
use gpui::{
    AnyElement, Context, Corner, ElementId, IntoElement, ParentElement, SharedString, Styled,
    Window, div, prelude::*, px,
};
use maestro_client::mirror::{AgentView, Entry, ToolState};
use maestro_core::{AgentId, NodeStatus, PermissionKind, ProviderKind, Workflow};
use maestro_protocol::AgentStatus;

use crate::app::{Maestro, Progress};
use crate::dropdown::{self, Menu};
use crate::graph::model_label;
use crate::i18n::tr;
use crate::{input, trf};

/// The widest the transcript and the composer grow, so lines stay easy to read on a wide screen.
const READING_WIDTH: f32 = 780.;

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

/// An element id for a transcript row.
fn element(kind: &str, key: &str) -> ElementId {
    ElementId::Name(format!("{kind}-{key}").into())
}

/// A reply with its plan blocks folded away: the plan follows the reply as a card, which
/// offers to open it.
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
        shown.push_str(tr("(The plan is below.)", "（計画は下にあります）"));
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

/// The mark Atelier draws for a provider's models, where it has one.
pub fn provider_logo(provider: ProviderKind) -> Option<ProviderLogo> {
    match provider {
        ProviderKind::ClaudeCode => Some(ProviderLogo::Anthropic),
        ProviderKind::Codex => Some(ProviderLogo::OpenAi),
        ProviderKind::Antigravity => Some(ProviderLogo::Google),
        // OpenCode fronts many vendors, so no single mark is right.
        ProviderKind::OpenCode => None,
    }
}

/// How a step reads in a plan's step list.
fn step_state(status: NodeStatus) -> StepState {
    match status {
        NodeStatus::Pending => StepState::Pending,
        NodeStatus::Running | NodeStatus::AwaitingUser | NodeStatus::AwaitingChoice => {
            StepState::Running
        }
        NodeStatus::Succeeded => StepState::Done,
        NodeStatus::Failed => StepState::Failed,
    }
}

impl Maestro {
    /// An agent's conversation as transcript rows.
    pub(crate) fn entries(
        &self,
        agent: AgentId,
        view: &AgentView,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        let logo = provider_logo(view.spec.model.provider);
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
                Entry::Assistant { text, done } => {
                    let text = without_plans(text);
                    // Only the person and this agent speak here, so the bubble already says
                    // who is who; the model moves to the footer.
                    AssistantMessage::new(text.clone())
                        .id(element("assistant", &key))
                        .anonymous()
                        .when_some(logo, AssistantMessage::logo)
                        .meta(meta.clone())
                        .streaming(!done)
                        .action(CopyButton::new(element("copy", &key), text))
                        .into_any_element()
                }
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
            rows.push(
                ThinkingIndicator::new(tr("Thinking", "考え中"))
                    .when_some(logo, ThinkingIndicator::logo)
                    .into_any_element(),
            );
        }
        // Sent during the turn; the agent reads them when it ends.
        for (index, text) in view.transcript.queued.iter().enumerate() {
            let key: SharedString = format!("{agent}-queued-{index}").into();
            rows.push(
                UserMessage::new(text.clone()).id(element("queued", &key)).into_any_element(),
            );
        }
        if !view.transcript.queued.is_empty() {
            rows.push(
                ChatNotice::status(tr(
                    "Sends when the current reply ends",
                    "現在の返答が終わったら送信します",
                ))
                .into_any_element(),
            );
        }
        rows
    }

    /// A workflow the commander planned, as a card in the conversation: its steps and where
    /// they are, with the commands to run it and to open its graph.
    fn plan_card(&self, flow: &Workflow, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let workflow = flow.id;
        let progress = Progress::of(flow);
        let mut steps = StepList::new(("plan-steps", workflow.0 as usize));
        for node in flow.topological_order().into_iter().filter_map(|id| flow.node(id)) {
            let state = step_state(node.status);
            steps = match node.status {
                NodeStatus::AwaitingUser => steps.step_with_detail(
                    node.title.clone(),
                    tr("Waiting for you", "あなた待ち"),
                    state,
                ),
                NodeStatus::AwaitingChoice => steps.step_with_detail(
                    node.title.clone(),
                    tr("Pick the better model's work", "良い方のモデルの成果を選んでください"),
                    state,
                ),
                NodeStatus::Running => {
                    steps.step_with_detail(node.title.clone(), model_label(node), state)
                }
                _ => steps.step(node.title.clone(), state),
            };
        }
        let summary =
            trf!("{} of {} steps done", "{} / {} ステップ完了", progress.done, progress.total);
        Card::new()
            .w_full()
            .p(theme.spacing.lg)
            .gap(theme.spacing.md)
            .bg(theme.colors.surface)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(theme.spacing.sm)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(SectionHeader::new(flow.title.clone()).description(summary)),
                    )
                    .when(progress.waiting > 0, |row| {
                        row.child(
                            Badge::new(trf!(
                                "{} waiting for you",
                                "{} 件があなた待ち",
                                progress.waiting
                            ))
                            .tone(Tone::Warning)
                            .icon(IconName::Alert),
                        )
                    })
                    .when(progress.failed > 0, |row| {
                        row.child(
                            Badge::new(trf!("{} failed", "{} 件失敗", progress.failed))
                                .tone(Tone::Danger)
                                .icon(IconName::Alert),
                        )
                    }),
            )
            .when(progress.total > 0, |card| card.child(steps))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(theme.spacing.sm)
                    .child(
                        Button::new(
                            ("open-plan", workflow.0 as usize),
                            tr("Open workflow", "ワークフローを開く"),
                        )
                        .variant(ButtonVariant::Agent)
                        .icon(IconName::Sparkles)
                        .size(ControlSize::Small)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_workflow(workflow);
                            cx.notify();
                        })),
                    )
                    .map(|row| {
                        if progress.finished {
                            row.child(
                                Button::new(
                                    ("publish-plan", workflow.0 as usize),
                                    tr("Open pull request", "プルリクエストを作成"),
                                )
                                .primary()
                                .icon(IconName::ArrowUp)
                                .size(ControlSize::Small)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.publish(workflow, cx)),
                                ),
                            )
                        } else {
                            row.child(
                                Button::new(("run-plan", workflow.0 as usize), tr("Run", "実行"))
                                    .primary()
                                    .icon(IconName::Play)
                                    .size(ControlSize::Small)
                                    .disabled(!progress.ready)
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.run(workflow, cx)),
                                    ),
                            )
                        }
                    }),
            )
    }

    /// What the conversation shows before anything is said.
    fn greeting(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        div()
            .flex()
            .flex_col()
            .gap(theme.spacing.xl)
            .pt(theme.spacing.xxl)
            .child(SectionHeader::new(tr("What should be built?", "何を作りますか？")).description(
                tr(
                    "The commander reads the repository, talks the plan through with you, and \
                     writes it as a workflow of steps; each step gets its own worker and its own \
                     git worktree. It never changes files itself.",
                    "コマンダーはリポジトリを読み、あなたと計画を話し合って、ステップからなる\
                     ワークフローとして書き出します。各ステップには専用のワーカーと git \
                     ワークツリーが割り当てられます。コマンダー自身がファイルを変更することは\
                     ありません。",
                ),
            ))
            .child(self.project_note(cx))
            .when(self.project.is_some(), |column| {
                column.child(
                    div().flex().child(
                        Button::new(
                            "greeting-empty",
                            tr("Start from an empty workflow", "空のワークフローから始める"),
                        )
                        .ghost()
                        .icon(IconName::Plus)
                        .size(ControlSize::Small)
                        .on_click(cx.listener(|this, _, _, cx| this.new_workflow(cx))),
                    ),
                )
            })
    }

    /// Under the greeting: what the open project holds, or how to make one.
    fn project_note(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let Some(project) = self.open_project() else {
            return div()
                .flex()
                .child(
                    Button::new("greeting-project", tr("New project", "新しいプロジェクト"))
                        .primary()
                        .icon(IconName::Plus)
                        .on_click(cx.listener(|this, _, _, cx| this.new_project(cx))),
                )
                .into_any_element();
        };
        let rows: Vec<_> = project
            .resources
            .iter()
            .filter(|entry| !matches!(entry.resource, maestro_core::Resource::Provider { .. }))
            .map(|entry| {
                let (label, line) = crate::project::resource_label(&entry.resource);
                Label::new(format!("{label} · {line}"))
                    .size(TextSize::Small)
                    .color(TextColor::Muted)
                    .truncate()
                    .into_any_element()
            })
            .collect();
        let providers = self.allowed_providers();
        let providers = if providers.len() == ProviderKind::ALL.len() {
            tr("Any AI provider", "すべての AI プロバイダ").to_string()
        } else {
            providers.iter().map(|provider| provider.label()).collect::<Vec<_>>().join(", ")
        };
        let empty = rows.is_empty();
        Card::new()
            .p(theme.spacing.md)
            .gap(theme.spacing.sm)
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div().flex_1().child(Label::caption(tr(
                            "What this project's commander works with",
                            "このプロジェクトのコマンダーが使うもの",
                        ))),
                    )
                    .child(
                        Button::new("greeting-resources", tr("Edit", "編集"))
                            .ghost()
                            .icon(IconName::Folder)
                            .size(ControlSize::Small)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.resources_open {
                                    this.toggle_resources(cx);
                                }
                            })),
                    ),
            )
            .children(rows)
            .when(empty, |card| {
                card.child(
                    Label::new(tr(
                        "No repository or folder yet. Add one so plans can run.",
                        "リポジトリもフォルダもまだありません。計画を実行するには追加してください。",
                    ))
                    .size(TextSize::Small)
                    .color(TextColor::Warning),
                )
            })
            .child(
                Label::new(trf!("Workers: {providers}", "ワーカー: {providers}"))
                    .size(TextSize::Small)
                    .color(TextColor::Muted),
            )
            .into_any_element()
    }

    /// The providers a new commander can run on: those the project allows.
    fn commander_menu(&self, current: ProviderKind, cx: &mut Context<Self>) -> Popover {
        let allowed = self.allowed_providers();
        Popover::new().id("commander-menu").w(px(220.)).children(allowed.into_iter().map(
            |provider| {
                dropdown::provider_item(("commander-provider", provider as usize), provider)
                    .checked(provider == current)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.provider = provider;
                        this.menu = None;
                        cx.notify();
                    }))
            },
        ))
    }

    /// The conversation with the commander: its transcript, the plans it wrote, and the
    /// composer, in one column centred in the space it is given.
    pub(crate) fn chat(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let view = self
            .conversation
            .and_then(|agent| Some((agent, self.mirror.agents.get(&agent)?.clone())));
        let provider =
            view.as_ref().map_or(self.commander_provider(), |(_, view)| view.spec.model.provider);
        let busy = view.as_ref().is_some_and(|(_, view)| view.transcript.busy);
        let fresh = view.is_none();

        let mut rows = Vec::new();
        if let Some((agent, view)) = &view {
            rows = self.entries(*agent, view, cx);
            let plans = self.mirror.plans.get(agent).cloned().unwrap_or_default();
            for workflow in plans {
                let Some(flow) = self.mirror.workflows.get(&workflow).cloned() else { continue };
                rows.push(self.plan_card(&flow, cx).into_any_element());
            }
        }
        // Follow the conversation while it grows.
        let length = (self.conversation, rows.len());
        if self.chat_length != length {
            self.chat_length = length;
            self.chat_scroll.scroll_to_bottom();
        }
        let empty = rows.is_empty();

        let composer = Composer::new("commander-composer")
            .input({
                let this = cx.entity().downgrade();
                input::message(&self.fields.composer, move |window, cx| {
                    this.update(cx, |this, cx| this.send_to_commander(window, cx)).ok();
                })
            })
            .model(provider.label())
            .model_logo(dropdown::provider_icon(provider))
            .busy(busy)
            // The daemon holds a message sent mid-turn until the turn ends.
            .queue(true)
            .on_send(cx.listener(|this, _, window, cx| this.send_to_commander(window, cx)))
            .on_stop(cx.listener(|this, _, _, cx| this.stop_commander(cx)))
            .when(fresh, |composer| {
                // The provider is fixed once the conversation starts.
                composer.on_model(cx.listener(|this, _, _, cx| {
                    this.menu = if this.menu == Some(Menu::Commander) {
                        None
                    } else {
                        Some(Menu::Commander)
                    };
                    cx.notify();
                }))
            });
        // The picker opens upward from the composer's bottom row, over the transcript.
        let menu = (fresh && self.menu == Some(Menu::Commander)).then(|| {
            div().absolute().bottom(px(42.)).left(px(44.)).child(dropdown::floating(
                "commander-menu-backdrop",
                self.commander_menu(provider, cx),
                Corner::BottomLeft,
                self.close_menu(cx),
                window,
            ))
        });

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme.colors.surface)
            .child(
                div()
                    .id("chat-transcript")
                    .flex()
                    .flex_col()
                    .items_center()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.chat_scroll)
                    .px(theme.spacing.xl)
                    .py(theme.spacing.xl)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(theme.spacing.lg)
                            .w_full()
                            .max_w(px(READING_WIDTH))
                            .children(rows)
                            .when(empty, |column| column.child(self.greeting(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_center()
                    .px(theme.spacing.xl)
                    .pb(theme.spacing.lg)
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .w_full()
                            .max_w(px(READING_WIDTH))
                            .child(composer)
                            .children(menu),
                    ),
            )
    }
}
