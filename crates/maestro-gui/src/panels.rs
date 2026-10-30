//! Everything around the conversation: the sidebar, the workflow pane that opens beside it with
//! its canvas and inspector, and the screens shown before the daemon answers.

use atelier::agent::Composer;
use atelier::{
    ActiveTheme, Badge, Button, ButtonVariant, Card, ControlSize, Edge, EmptyState, IconName,
    Label, ListItem, SectionHeader, Segment, SegmentedControl, Sidebar, Spinner, Switch, Tabs,
    TextColor, TextField, TextSize, Tone,
};
use gpui::{Context, IntoElement, ParentElement, Pixels, Styled, div, prelude::*, px};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use maestro_client::mirror::AgentView;
use maestro_core::{
    AgentId, Difficulty, ModelChoice, NodeEdit, NodeId, NodeOrigin, NodeRef, NodeStatus,
    ProviderKind,
};

use crate::app::{Maestro, Progress};
use crate::chat::provider_logo;
use crate::graph::{model_label, status_label, status_tone};
use crate::i18n::{self, tr};
use crate::project::{host_label, project_summary, resource_label};
use crate::{input, link, trf};

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

    /// The projects, with the open project's workflows under it.
    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let mut rows = Vec::new();
        for project in self.mirror.projects.values().rev() {
            let id = project.id;
            let view = cx.entity();
            let open = self.project == Some(id);
            let flows: Vec<_> = self.mirror.workflows_of(id).rev().collect();
            let waiting = flows.iter().any(|flow| Progress::of(flow).waiting > 0);
            let working = project
                .commander
                .and_then(|agent| self.mirror.agents.get(&agent))
                .is_some_and(|view| view.transcript.busy);
            let row = ListItem::new(("project", id.0 as usize), project.name.clone())
                .icon(if waiting { IconName::Alert } else { IconName::Folder })
                .when(waiting, |item| item.icon_color(theme.colors.warning))
                .description(if working {
                    tr("The commander is working", "コマンダーが作業中").to_string()
                } else {
                    project_summary(
                        project
                            .resources
                            .iter()
                            .filter(|entry| {
                                !matches!(entry.resource, maestro_core::Resource::Provider { .. })
                            })
                            .count(),
                        flows.len(),
                    )
                })
                .selected(open && !self.workflow_open)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.enter_project(id);
                    this.workflow_open = false;
                    this.workflow_full = false;
                    cx.notify();
                }));
            rows.push(
                div()
                    .id(("project-menu", id.0 as usize))
                    .child(div().child(row).context_menu(move |menu, window, cx| {
                        let open_view = view.clone();
                        let resources_view = view.clone();
                        let delete_view = view.clone();
                        menu.min_w(cx.theme().typography.size_md * 16.)
                            .when_some(window.focused(cx), |menu, focus| menu.action_context(focus))
                            .item(PopupMenuItem::new(tr("Open", "開く")).on_click(
                                move |_, _, cx| {
                                    open_view.update(cx, |this, cx| {
                                        this.enter_project(id);
                                        this.workflow_open = false;
                                        this.workflow_full = false;
                                        this.resources_open = false;
                                        cx.notify();
                                    });
                                },
                            ))
                            .item(
                                PopupMenuItem::new(tr("Show resources", "リソースを表示"))
                                    .on_click(move |_, _, cx| {
                                        resources_view.update(cx, |this, cx| {
                                            this.enter_project(id);
                                            this.resources_open = true;
                                            this.workflow_open = false;
                                            this.workflow_full = false;
                                            cx.notify();
                                        });
                                    }),
                            )
                            .separator()
                            .item(
                                PopupMenuItem::element(|_, _| {
                                    Label::new(tr("Delete project…", "プロジェクトを削除…"))
                                        .color(TextColor::Danger)
                                })
                                .on_click(move |_, window, cx| {
                                    let view = delete_view.clone();
                                    window.defer(cx, move |window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.delete_project(id, window, cx)
                                        });
                                    });
                                }),
                            )
                    }))
                    .into_any_element(),
            );
            if !open {
                continue;
            }
            for flow in flows {
                let workflow = flow.id;
                let progress = Progress::of(flow);
                let attention = progress.waiting > 0 || progress.failed > 0;
                rows.push(
                    ListItem::new(("workflow", workflow.0 as usize), flow.title.clone())
                        .indent(1)
                        .icon(if attention { IconName::Alert } else { IconName::Sliders })
                        .when(attention, |item| item.icon_color(theme.colors.warning))
                        .description(match progress.total {
                            0 => tr("No steps", "ステップなし").to_string(),
                            total => trf!(
                                "{} of {total} steps done",
                                "{total} ステップ中 {} 完了",
                                progress.done
                            ),
                        })
                        .selected(self.workflow_open && self.workflow == Some(workflow))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_workflow(workflow);
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
        }
        let none = rows.is_empty();
        let has_project = self.project.is_some();
        Sidebar::new(Edge::Left)
            .w(px(crate::app::SIDEBAR_WIDTH))
            .p(theme.spacing.sm)
            .gap(theme.spacing.xxs)
            .child(
                Button::new("new-project", tr("New project", "新しいプロジェクト"))
                    .icon(IconName::Plus)
                    .full_width()
                    .on_click(cx.listener(|this, _, _, cx| this.new_project(cx))),
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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .child(self.caption(tr("Projects", "プロジェクト"), cx)),
                            )
                            .when(has_project, |row| {
                                row.child(
                                    Button::icon_only("new-workflow", IconName::Plus)
                                        .size(ControlSize::Small)
                                        .tooltip(tr(
                                            "New empty workflow in this project",
                                            "このプロジェクトに空のワークフローを作成",
                                        ))
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.new_workflow(cx)),
                                        ),
                                )
                            }),
                    )
                    .children(rows)
                    .when(none, |list| {
                        list.child(
                            div().px(theme.spacing.sm).child(
                                Label::new(tr(
                                    "A project gathers the repositories, machines and AI \
                                     providers one commander works with",
                                    "プロジェクトには、1 体のコマンダーが使うリポジトリ・\
                                     マシン・AI プロバイダをまとめます",
                                ))
                                .size(TextSize::XSmall)
                                .color(TextColor::Faint),
                            ),
                        )
                    }),
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
                        Label::new(trf!(
                            "{} of {} steps done",
                            "{} / {} ステップ完了",
                            progress.done,
                            progress.total
                        ))
                        .size(TextSize::XSmall)
                        .color(TextColor::Muted)
                        .truncate(),
                    ),
            )
            .when(progress.waiting > 0, |bar| {
                bar.child(
                    Badge::new(trf!("{} waiting for you", "{} 件があなた待ち", progress.waiting))
                        .tone(Tone::Warning)
                        .icon(IconName::Alert),
                )
            })
            .when(progress.failed > 0, |bar| {
                bar.child(
                    Badge::new(trf!("{} failed", "{} 件失敗", progress.failed))
                        .tone(Tone::Danger)
                        .icon(IconName::Alert),
                )
            })
            .child(if progress.finished {
                Button::new("publish", tr("Open pull request", "プルリクエストを作成"))
                    .primary()
                    .icon(IconName::ArrowUp)
                    .size(ControlSize::Small)
                    .on_click(cx.listener(move |this, _, _, cx| this.publish(workflow, cx)))
            } else {
                Button::new("run", tr("Run", "実行"))
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
                        tr("Show the conversation beside it", "会話を横に表示")
                    } else {
                        tr("Fill the window", "ウィンドウ全体に広げる")
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workflow_full = !this.workflow_full;
                        cx.notify();
                    })),
                )
            })
            .child(
                Button::icon_only("close-workflow", IconName::Close)
                    .tooltip(tr("Back to the conversation", "会話に戻る"))
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
        // A contender the user asked to see, else the node's own worker.
        let worker = node
            .as_ref()
            .and_then(|node| {
                let contender = self.contender.filter(|agent| {
                    node.contenders.iter().any(|contender| contender.agent == *agent)
                });
                contender.or(node.agent)
            })
            .and_then(|agent| Some((agent, self.mirror.agents.get(&agent)?.clone())));
        let content = match &node {
            Some(node) if self.node_tab == 1 && worker.is_some() => {
                let (agent, view) = worker.clone().expect("checked above");
                // Only the node's own worker takes revision requests.
                let reply = node.agent == Some(agent);
                self.worker(agent, &view, width, reply, cx).into_any_element()
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
                .child(
                    Tabs::new(
                        "node-tabs",
                        [tr("Details", "詳細"), tr("Worker", "ワーカー")],
                        self.node_tab,
                    )
                    .on_select(cx.listener(|this, index: &usize, _, cx| {
                        this.node_tab = *index;
                        cx.notify();
                    })),
                )
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
        // Earlier work it waits for, in other workflows.
        let waiting: Vec<(NodeRef, String)> = node
            .after
            .iter()
            .map(|&on| {
                let earlier = self.mirror.workflows.get(&on.workflow);
                let title = earlier.and_then(|flow| Some((flow, flow.node(on.node)?)));
                let title = match title {
                    Some((flow, node)) => {
                        trf!(
                            "{} (in \u{201c}{}\u{201d})",
                            "{}（\u{300c}{}\u{300d}）",
                            node.title,
                            flow.title
                        )
                    }
                    None => on.to_string(),
                };
                (on, title)
            })
            .collect();
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
                    )
                    .when(!node.tags.is_empty(), |group| {
                        group.child(
                            Label::new(trf!("Kind of work: {}", "作業の種類: {}", node.tags.join(", ")))
                                .size(TextSize::XSmall)
                                .color(TextColor::Muted),
                        )
                    }),
            )
            .when(!node.contenders.is_empty(), |details| details.child(self.contenders(node, cx)))
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
                    .when(dependencies.is_empty() && waiting.is_empty(), |list| {
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
                    }))
                    .children(waiting.into_iter().enumerate().map(|(index, (on, title))| {
                        ListItem::new(("waiting", index), title)
                            .icon(IconName::ChevronRight)
                            .trailing(
                                Button::icon_only(("drop-waiting", index), IconName::Close)
                                    .size(ControlSize::Small)
                                    .tooltip(tr("Stop waiting for this step", "このステップを待たない"))
                                    .on_click(cx.listener(move |this, _, _, cx| this.stop_waiting(id, on, cx))),
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
        agent: AgentId,
        view: &AgentView,
        width: Pixels,
        reply: bool,
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
            .when(reply, |worker| {
                worker.child(
                    div().flex_none().px(theme.spacing.sm).pb(theme.spacing.sm).child(
                        input::stretch(
                            width - theme.spacing.sm * 2.,
                            Composer::new("worker-composer")
                                .input({
                                    let this = cx.entity().downgrade();
                                    input::message(&self.fields.node_message, move |window, cx| {
                                        this.update(cx, |this, cx| this.send_to_node(window, cx))
                                            .ok();
                                    })
                                })
                                .model(trf!("{} · worker", "{} · ワーカー", provider.label()))
                                .when_some(provider_logo(provider), Composer::model_logo)
                                .busy(busy)
                                // The daemon holds a message sent mid-turn until the turn ends.
                                .queue(true)
                                .on_send(
                                    cx.listener(|this, _, window, cx| {
                                        this.send_to_node(window, cx)
                                    }),
                                )
                                .on_stop(
                                    cx.listener(move |this, _, _, cx| this.interrupt(agent, cx)),
                                ),
                        ),
                    ),
                )
            })
    }

    /// The models a node ran on at once, each with the end of its answer, to pick the better
    /// work.
    fn contenders(
        &self,
        node: &maestro_core::Node,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let id = node.id;
        let choosing = node.status == NodeStatus::AwaitingChoice;
        let hint = if choosing {
            tr(
                "Pick the better work. Maestro learns which model does well at this kind of work",
                "良い方の成果を選んでください。Maestro はこの種類の作業が得意なモデルを学習します",
            )
        } else if node.agent.is_some() {
            tr(
                "The kept work is the one its dependents build on",
                "採用した成果を、後続のステップが引き継ぎます",
            )
        } else {
            tr(
                "Each model works in its own worktree; pick one when they finish",
                "各モデルが別々の worktree で作業しています。終わったら1つ選んでください",
            )
        };
        let cards = node.contenders.iter().enumerate().map(|(index, contender)| {
            let agent = contender.agent;
            let kept = node.agent == Some(agent);
            let (badge, tone) = if kept {
                (tr("Kept", "採用"), Tone::Success)
            } else {
                (status_label(contender.status), status_tone(contender.status))
            };
            let answer =
                self.mirror.agents.get(&agent).and_then(|view| view.transcript.last_answer()).map(
                    |answer| match answer.char_indices().nth(280) {
                        Some((end, _)) => format!("{}…", &answer[..end]),
                        None => answer.to_owned(),
                    },
                );
            Card::new()
                .p(theme.spacing.md)
                .gap(theme.spacing.sm)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(theme.spacing.sm)
                        .child(
                            Label::new(format!(
                                "{} · {}",
                                contender.model.provider.label(),
                                crate::graph::choice_label(&contender.model)
                            ))
                            .size(TextSize::Small),
                        )
                        .child(Badge::new(badge).tone(tone).dot()),
                )
                .children(answer.map(|answer| {
                    Label::new(answer).size(TextSize::XSmall).color(TextColor::Muted)
                }))
                .child(
                    Label::new(contender.branch.clone())
                        .size(TextSize::XSmall)
                        .color(TextColor::Faint),
                )
                .child(
                    div()
                        .flex()
                        .gap(theme.spacing.xs)
                        .child(
                            Button::new(("view-contender", index), tr("View work", "成果を見る"))
                                .icon(IconName::Eye)
                                .size(ControlSize::Small)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.view_contender(agent, cx)
                                })),
                        )
                        .when(choosing && contender.status == NodeStatus::Succeeded, |row| {
                            row.child(
                                Button::new(
                                    ("keep-contender", index),
                                    tr("Keep this", "これを採用"),
                                )
                                .primary()
                                .icon(IconName::Check)
                                .size(ControlSize::Small)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.choose_contender(id, agent, cx),
                                )),
                            )
                        }),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(theme.spacing.xs)
            .child(Label::new(tr("Compared", "比較")).size(TextSize::Small).color(TextColor::Muted))
            .children(cards)
            .child(Label::new(hint).size(TextSize::XSmall).color(TextColor::Faint))
    }

    /// The inspector with no node selected: the workflow as a whole.
    fn overview(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let Some(flow) = self.current() else { return div().into_any_element() };
        let total = flow.nodes().count();
        let done = flow.nodes().filter(|node| node.status == NodeStatus::Succeeded).count();
        let publish = flow.target.as_ref().is_some_and(|target| target.publish);
        let target = flow.target.as_ref().map(|target| {
            trf!(
                "{} on {}, from {}",
                "{}（{}・{} から）",
                target.repository.display(),
                host_label(&target.host),
                target.base
            )
        });
        let workflow = flow.id;
        // Every place the project offers: each folder on its machine, and each GitHub
        // repository on the daemon's machine and on each of the project's machines.
        let mut places = Vec::new();
        if let Some(project) = flow.project.and_then(|id| self.mirror.projects.get(&id)) {
            let machines: Vec<_> = project
                .resources
                .iter()
                .filter_map(|entry| match &entry.resource {
                    maestro_core::Resource::Machine { destination } => {
                        Some((entry.id, destination.clone()))
                    }
                    _ => None,
                })
                .collect();
            for entry in project.resources.iter().filter(|entry| entry.resource.is_repository()) {
                let (label, line) = resource_label(&entry.resource);
                let github = matches!(entry.resource, maestro_core::Resource::GitHub { .. });
                let mut choices = vec![(None, line)];
                if github {
                    choices[0].1 =
                        trf!("{} on the daemon's machine", "{}・デーモンのマシン", choices[0].1);
                    for (machine, destination) in &machines {
                        let line = trf!("GitHub, on {destination}", "GitHub・{destination}");
                        choices.push((Some(*machine), line));
                    }
                }
                for (machine, line) in choices {
                    let resource = entry.id;
                    let index = places.len();
                    places.push(
                        ListItem::new(("place", index), label.clone())
                            .icon(if github { IconName::Code } else { IconName::Folder })
                            .description(line)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.place(workflow, resource, machine, cx)
                            }))
                            .into_any_element(),
                    );
                }
            }
        }
        let no_places = places.is_empty();
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
                                "Not chosen yet. Run uses the project's only repository; pick one \
                                 below when it has several.",
                                "未選択です。リポジトリが 1 つだけなら実行時にそれを使います。\
                                 複数あるときは下から選んでください。",
                            )
                            .into()
                        }))
                        .size(TextSize::Small)
                        .color(TextColor::Muted),
                    )
                    .when(!no_places, |column| {
                        column.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(theme.spacing.xxs)
                                .child(
                                    Label::new(tr(
                                        "Run it in one of the project's repositories",
                                        "プロジェクトのリポジトリから選ぶ",
                                    ))
                                    .size(TextSize::XSmall)
                                    .color(TextColor::Faint),
                                )
                                .children(places),
                        )
                    })
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
