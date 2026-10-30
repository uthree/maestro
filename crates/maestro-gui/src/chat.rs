//! The conversation with the commander, which fills the middle of the window. The workflow it
//! plans is shown in the chat as a card and opens beside it on demand.

use atelier::agent::{
    ApprovalCard, AssistantMessage, ChatNotice, Composer, ReasoningBlock, ThinkingIndicator,
    ToolCall, ToolGroup, UserMessage,
};
use atelier::{
    ActiveTheme, Badge, Button, ButtonVariant, Card, ControlSize, CopyButton, IconName,
    ProviderLogo, SectionHeader, StepList, StepState, Tone,
};
use gpui::{
    AnyElement, Context, ElementId, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use maestro_client::mirror::{AgentView, Entry, ToolState};
use maestro_core::{AgentId, NodeStatus, PermissionKind, ProviderKind, Workflow};
use maestro_protocol::AgentStatus;

use crate::app::{Maestro, Progress};
use crate::graph::model_label;
use crate::input;

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

/// A conversation's name: the start of its first message.
pub fn conversation_title(view: &AgentView) -> String {
    let first = view.transcript.entries.iter().find_map(|entry| match entry {
        Entry::User(text) => Some(text.as_str()),
        _ => None,
    });
    let Some(first) = first else { return "New conversation".into() };
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
        shown.push_str("(The plan is below.)");
        rest = &body[end + 3..];
    }
    shown.push_str(rest);
    shown
}

/// What allowing a request of this kind lets the agent do.
fn approval_body(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Command => "The agent wants to run this command in its worktree.",
        PermissionKind::FileChange => "The agent wants to change files in its worktree.",
        PermissionKind::Other => "The agent wants to do something its permissions do not cover.",
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
        NodeStatus::Running | NodeStatus::AwaitingUser => StepState::Running,
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
                Entry::Reasoning(text) => {
                    ReasoningBlock::new(element("reasoning", &key), "Reasoning", text.clone())
                        .open(open)
                        .on_toggle(toggle)
                        .into_any_element()
                }
                Entry::Tools(tools) => {
                    let title = match tools.len() {
                        1 => "Used 1 tool".to_string(),
                        count => format!("Used {count} tools"),
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
                        format!("Allow: {}?", request.summary),
                    )
                    .body(approval_body(request.kind))
                    .allow_label("Allow")
                    .on_allow(cx.listener(move |this, _, _, cx| {
                        this.answer(agent, allow.clone(), true, cx);
                    }))
                    .on_deny(cx.listener(move |this, _, _, cx| {
                        this.answer(agent, deny.clone(), false, cx);
                    }))
                    .into_any_element()
                }
                Entry::Permission { request, allowed, .. } => ChatNotice::status(match allowed {
                    Some(true) => format!("Allowed: {}", request.summary),
                    _ => format!("Refused: {}", request.summary),
                })
                .into_any_element(),
                Entry::Failed(message) => {
                    ChatNotice::error(format!("The turn failed: {message}")).into_any_element()
                }
                Entry::Exited(reason) => ChatNotice::status(match reason {
                    Some(reason) => format!("The agent stopped: {reason}"),
                    None => "The agent stopped".into(),
                })
                .into_any_element(),
            };
            rows.push(row);
        }
        if view.transcript.busy && matches!(view.transcript.entries.last(), Some(Entry::User(_))) {
            rows.push(
                ThinkingIndicator::new("Thinking")
                    .when_some(logo, ThinkingIndicator::logo)
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
                NodeStatus::AwaitingUser => {
                    steps.step_with_detail(node.title.clone(), "Waiting for you", state)
                }
                NodeStatus::Running => {
                    steps.step_with_detail(node.title.clone(), model_label(node), state)
                }
                _ => steps.step(node.title.clone(), state),
            };
        }
        let summary = format!("{} of {} steps done", progress.done, progress.total);
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
                            Badge::new(format!("{} waiting for you", progress.waiting))
                                .tone(Tone::Warning)
                                .icon(IconName::Alert),
                        )
                    })
                    .when(progress.failed > 0, |row| {
                        row.child(
                            Badge::new(format!("{} failed", progress.failed))
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
                        Button::new(("open-plan", workflow.0 as usize), "Open workflow")
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
                                    "Open pull request",
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
                                Button::new(("run-plan", workflow.0 as usize), "Run")
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
            .child(SectionHeader::new("What should be built?").description(
                "The commander reads the repository, talks the plan through with you, and \
                 writes it as a workflow of steps; each step gets its own worker and its own git \
                 worktree. It never changes files itself.",
            ))
            .when(self.project.repository.is_none(), |empty| empty.child(self.project_fields(cx)))
            .child(
                div().flex().child(
                    Button::new("greeting-empty", "Start from an empty workflow")
                        .ghost()
                        .icon(IconName::Plus)
                        .size(ControlSize::Small)
                        .on_click(cx.listener(|this, _, _, cx| this.new_workflow(cx))),
                ),
            )
    }

    /// The conversation with the commander: its transcript, the plans it wrote, and the
    /// composer, in one column centred in the space it is given.
    pub(crate) fn chat(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let view = self
            .conversation
            .and_then(|agent| Some((agent, self.mirror.agents.get(&agent)?.clone())));
        let provider = view.as_ref().map_or(self.provider, |(_, view)| view.spec.model.provider);
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
            .when_some(provider_logo(provider), Composer::model_logo)
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
                        div().flex().flex_col().w_full().max_w(px(READING_WIDTH)).child(composer),
                    ),
            )
    }
}
