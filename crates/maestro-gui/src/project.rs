//! The open project's resources, in the pane beside the conversation: its name, the
//! repositories, folders and machines its commander may choose from, and the AI providers its
//! work may run on. Also what a right-click on a project in the sidebar offers, and the dialogs
//! that rename and delete one.

use atelier::{
    ActiveTheme, Button, ButtonVariant, ControlSize, FormField, FormGroup, IconName, Label,
    ListItem, MenuItem, Popover, SectionHeader, Segment, SegmentedControl, Switch, TextColor,
    TextField, TextSize, menu_separator,
};
use gpui::{
    AnyElement, Context, IntoElement, ParentElement, Pixels, Point, Styled, Window, div,
    prelude::*, px,
};
use maestro_core::{Host, ProjectId, ProviderKind, Resource};

use crate::app::Maestro;
use crate::dialog::{self, Dialog, Parts};
use crate::dropdown;
use crate::i18n::tr;
use crate::{input, trf};

/// A machine as the pane names it.
pub fn host_label(host: &Host) -> String {
    match host {
        Host::Local => tr("The daemon's machine", "デーモンのマシン").into(),
        Host::Ssh { destination } => destination.clone(),
    }
}

/// A resource as a list row names it: its label and a line under it.
pub fn resource_label(resource: &Resource) -> (String, String) {
    match resource {
        Resource::GitHub { repository, base } => {
            (repository.clone(), trf!("GitHub, from {base}", "GitHub・{base} から"))
        }
        Resource::Directory { host, path, base } => (
            path.display().to_string(),
            trf!("On {}, from {base}", "{}・{base} から", host_label(host)),
        ),
        Resource::Machine { destination } => {
            (destination.clone(), tr("SSH machine", "SSH 接続先").into())
        }
        Resource::Provider { provider } => (provider.label().into(), String::new()),
    }
}

fn resource_icon(resource: &Resource) -> IconName {
    match resource {
        Resource::GitHub { .. } => IconName::Code,
        Resource::Directory { .. } => IconName::Folder,
        Resource::Machine { .. } => IconName::Bolt,
        Resource::Provider { .. } => IconName::Sparkles,
    }
}

impl Maestro {
    /// The project pane, `width` wide.
    pub(crate) fn project_pane(&self, width: Pixels, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(project) = self.open_project() else { return div().into_any_element() };
        let id = project.id;
        let workflows = self.mirror.workflows_of(id).count();

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
                        Label::new(project.name.clone())
                            .weight(theme.typography.label_weight)
                            .truncate(),
                    )
                    .child(
                        Label::new(tr("Project resources", "プロジェクトのリソース"))
                            .size(TextSize::XSmall)
                            .color(TextColor::Muted),
                    ),
            )
            .child(
                Button::icon_only("close-resources", IconName::Close)
                    .tooltip(tr("Back to the conversation", "会話に戻る"))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_resources(cx))),
            );

        let places: Vec<_> = project
            .resources
            .iter()
            .filter(|entry| !matches!(entry.resource, Resource::Provider { .. }))
            .map(|entry| {
                let id = entry.id;
                let (label, description) = resource_label(&entry.resource);
                ListItem::new(("resource", id.0 as usize), label)
                    .icon(resource_icon(&entry.resource))
                    .description(format!("{id} · {description}"))
                    .trailing(
                        Button::icon_only(("remove-resource", id.0 as usize), IconName::Close)
                            .size(ControlSize::Small)
                            .tooltip(tr("Remove from the project", "プロジェクトから外す"))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_resource(id, cx)),
                            ),
                    )
                    .into_any_element()
            })
            .collect();
        let no_places = places.is_empty();

        let kinds = [
            Segment::label("GitHub"),
            Segment::label(tr("Folder", "フォルダ")),
            Segment::label(tr("SSH machine", "SSH 接続先")),
        ];
        let (what, hint) = match self.resource_kind {
            0 => (
                tr("Repository", "リポジトリ"),
                tr(
                    "owner/name or its URL. It is cloned with gh onto the machine a plan runs on.",
                    "owner/name か URL。計画を実行するマシンに gh で clone されます。",
                ),
            ),
            1 => (
                tr("Path", "パス"),
                tr(
                    "A git checkout on that machine. The commander reads the first folder to plan.",
                    "そのマシン上の git チェックアウト。コマンダーは最初のフォルダを読んで計画します。",
                ),
            ),
            _ => (
                tr("Destination", "接続先"),
                tr(
                    "Anything ssh accepts: user@host, or a Host from your ssh config.",
                    "ssh に渡せる接続先（user@host や ssh config の Host 名）。",
                ),
            ),
        };
        let hosts = self.resource_hosts();
        let host = self.resource_host.min(hosts.len() - 1);
        let add = FormGroup::new(tr("Add a resource", "リソースを追加"))
            .child(
                SegmentedControl::new("resource-kind", kinds, self.resource_kind)
                    .size(ControlSize::Small)
                    .on_select(cx.listener(|this, index: &usize, window, cx| {
                        this.resource_kind = *index;
                        let placeholder = match index {
                            0 => "owner/name",
                            1 => "/path/to/repository",
                            _ => "user@host",
                        };
                        this.fields
                            .resource
                            .update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
                        cx.notify();
                    })),
            )
            .when(self.resource_kind == 1 && hosts.len() > 1, |group| {
                group.child(
                    FormField::new(tr("Machine", "マシン")).child(
                        SegmentedControl::new(
                            "resource-host",
                            hosts.iter().map(|host| Segment::label(host_label(host))),
                            host,
                        )
                        .size(ControlSize::Small)
                        .on_select(cx.listener(
                            |this, index: &usize, _, cx| {
                                this.resource_host = *index;
                                cx.notify();
                            },
                        )),
                    ),
                )
            })
            .child(
                FormField::new(what)
                    .description(hint)
                    .child(TextField::new().child(input::editor(&self.fields.resource)))
                    .when(self.resource_kind == 1, |field| {
                        // The system's dialog shows this computer's folders, so it can only
                        // pick one for a daemon that runs here.
                        let here = self.folder_is_here(hosts.get(host));
                        field.child(
                            Button::new("choose-folder", tr("Choose…", "選択…"))
                                .icon(IconName::Folder)
                                .size(ControlSize::Small)
                                .disabled(!here)
                                .tooltip(if here {
                                    tr("Pick the folder in a dialog", "ダイアログでフォルダを選ぶ")
                                } else {
                                    tr(
                                        "A folder on another machine is typed in",
                                        "別のマシンのフォルダはパスを入力してください",
                                    )
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_folder(window, cx)
                                })),
                        )
                    }),
            )
            .when(self.resource_kind < 2, |group| {
                group.child(
                    FormField::new(tr("Base branch", "ベースブランチ"))
                        .child(TextField::new().child(input::editor(&self.fields.resource_base))),
                )
            })
            .child(
                div().flex().child(
                    Button::new("add-resource", tr("Add", "追加"))
                        .primary()
                        .icon(IconName::Plus)
                        .size(ControlSize::Small)
                        .on_click(cx.listener(|this, _, window, cx| this.add_resource(window, cx))),
                ),
            );

        let allowed = project.providers();
        let switches: Vec<_> = ProviderKind::ALL
            .into_iter()
            .enumerate()
            .map(|(index, provider)| {
                let on = project.allows(provider);
                // A project always allows at least one provider.
                let last = allowed.as_ref().is_some_and(|allowed| allowed == &[provider]);
                FormField::new(provider.label())
                    .beside()
                    .child(Switch::new(("allow-provider", index), on).disabled(last).on_click(
                        cx.listener(move |this, _, _, cx| this.allow_provider(provider, !on, cx)),
                    ))
                    .into_any_element()
            })
            .collect();

        let body = div()
            .id("project-pane")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap(theme.spacing.xl)
            .p(theme.spacing.lg)
            .child(SectionHeader::new(tr("Project", "プロジェクト")).description(tr(
                "Everything the commander may work with. It picks what each plan uses, and asks \
                 you when the choice is unclear.",
                "コマンダーが使えるもの一式です。計画ごとに何を使うかを選び、迷うときはあなたに\
                 相談します。",
            )))
            .child(
                FormGroup::untitled().child(
                    FormField::new(tr("Name", "名前"))
                        .child(TextField::new().child(input::editor(&self.fields.project_name)))
                        .child(
                            Button::new("rename-project", tr("Rename", "名前を変更"))
                                .size(ControlSize::Small)
                                .on_click(cx.listener(|this, _, _, cx| this.rename_project(cx))),
                        ),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.xxs)
                    .child(Label::caption(tr(
                        "Repositories, folders and machines",
                        "リポジトリ・フォルダ・マシン",
                    )))
                    .children(places)
                    .when(no_places, |list| {
                        list.child(
                            Label::new(tr(
                                "Nothing yet. Plans can be drawn, but run nowhere until you add a \
                                 repository or a folder.",
                                "まだありません。リポジトリかフォルダを追加するまで、計画は\
                                 描けても実行できません。",
                            ))
                            .size(TextSize::Small)
                            .color(TextColor::Faint),
                        )
                    }),
            )
            .child(add)
            .child(
                FormGroup::new(tr("AI providers", "AI プロバイダ"))
                    .description(tr(
                        "What the commander and workers may run on. Turn one off to keep this \
                         project's work away from it.",
                        "コマンダーとワーカーが使ってよいプロバイダです。オフにすると、この\
                         プロジェクトの作業には使われません。",
                    ))
                    .children(switches),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.sm)
                    .pt(theme.spacing.md)
                    .border_t_1()
                    .border_color(theme.colors.separator)
                    .child(
                        Label::new(trf!(
                            "Deleting the project deletes its {workflows} workflows too. Their \
                             branches and worktrees stay.",
                            "プロジェクトを削除すると、ワークフロー {workflows} 件も削除されます。\
                             ブランチとワークツリーは残ります。"
                        ))
                        .size(TextSize::Small)
                        .color(TextColor::Muted),
                    )
                    .child(
                        div().flex().child(
                            Button::new(
                                "delete-project",
                                tr("Delete project…", "プロジェクトを削除…"),
                            )
                            .variant(ButtonVariant::Danger)
                            .icon(IconName::Trash)
                            .size(ControlSize::Small)
                            .on_click(cx.listener(
                                move |this, _, window, cx| this.ask_delete(id, window, cx),
                            )),
                        ),
                    ),
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
            .bg(theme.colors.surface)
            .child(header)
            .child(body)
            .into_any_element()
    }
}

impl Maestro {
    /// The menu a right-click on `project` in the sidebar opens at `position`: open it, start a
    /// workflow in it, show its resources, stop its commander, rename it, delete it.
    pub(crate) fn project_menu(
        &self,
        project: ProjectId,
        position: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let found = self.mirror.projects.get(&project)?;
        let working = found
            .commander
            .and_then(|agent| self.mirror.agents.get(&agent))
            .is_some_and(|view| view.transcript.busy);
        let mut menu = Popover::new()
            .id("project-menu")
            .w(px(240.))
            .child(MenuItem::new("project-menu-open", tr("Open", "開く")).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.menu = None;
                    this.enter_project(project);
                    this.workflow_open = false;
                    this.workflow_full = false;
                    cx.notify();
                },
            )))
            .child(
                MenuItem::new("project-menu-workflow", tr("New workflow", "新しいワークフロー"))
                    .icon(IconName::Plus)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.enter_project(project);
                        this.new_workflow(cx);
                    })),
            )
            .child(
                MenuItem::new("project-menu-resources", tr("Resources", "リソース"))
                    .icon(IconName::Folder)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.enter_project(project);
                        if !this.resources_open {
                            this.toggle_resources(cx);
                        }
                        cx.notify();
                    })),
            );
        if working {
            menu = menu.child(
                MenuItem::new("project-menu-stop", tr("Stop the commander", "コマンダーを止める"))
                    .icon(IconName::Stop)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.stop_project_commander(project, cx);
                        cx.notify();
                    })),
            );
        }
        let menu =
            menu.child(menu_separator(cx))
                .child(MenuItem::new("project-menu-rename", tr("Rename…", "名前を変更…")).on_click(
                    cx.listener(move |this, _, window, cx| this.ask_rename(project, window, cx)),
                ))
                .child(
                    MenuItem::new("project-menu-delete", tr("Delete…", "削除…"))
                        .icon(IconName::Trash)
                        .danger()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.ask_delete(project, window, cx)
                        })),
                );
        Some(dropdown::at_pointer(
            "project-menu-backdrop",
            menu,
            position,
            self.close_menu(cx),
            window,
        ))
    }

    /// The dialog that is open over the window, if any.
    pub(crate) fn dialog(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.dialog?;
        let project = self.mirror.projects.get(&open.project())?;
        let name = project.name.clone();
        let this = cx.entity().downgrade();
        let cancel = move |_: &mut Window, cx: &mut gpui::App| {
            this.update(cx, |this, cx| this.close_dialog(cx)).ok();
        };
        let cancel_button = Button::new("dialog-cancel", tr("Cancel", "キャンセル"))
            .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx)))
            .into_any_element();
        let parts = match open {
            Dialog::Rename(_) => {
                let commit = cx.entity().downgrade();
                Parts {
                    title: tr("Rename project", "プロジェクトの名前を変更").into(),
                    message: None,
                    body: Some(
                        TextField::new()
                            .child(input::message(&self.fields.rename, move |_, cx| {
                                commit.update(cx, |this, cx| this.confirm_rename(cx)).ok();
                            }))
                            .into_any_element(),
                    ),
                    error: self.error.clone(),
                    actions: vec![
                        cancel_button,
                        Button::new("dialog-rename", tr("Rename", "名前を変更"))
                            .primary()
                            .on_click(cx.listener(|this, _, _, cx| this.confirm_rename(cx)))
                            .into_any_element(),
                    ],
                }
            }
            Dialog::Delete(id) => {
                let workflows = self.mirror.workflows_of(id).count();
                Parts {
                    title: trf!("Delete “{name}”?", "「{name}」を削除しますか？"),
                    message: Some(match workflows {
                        0 => tr(
                            "This can't be undone. Its folders and repositories are left as                              they are.",
                            "元に戻せません。フォルダとリポジトリはそのまま残ります。",
                        )
                        .into(),
                        _ => trf!(
                            "Its {workflows} workflows are deleted too, and this can't be                              undone. Their branches and worktrees stay.",
                            "ワークフロー {workflows} 件も削除され、元に戻せません。ブランチと                             ワークツリーは残ります。"
                        ),
                    }),
                    body: None,
                    error: self.error.clone(),
                    actions: vec![
                        cancel_button,
                        Button::new("dialog-delete", tr("Delete", "削除"))
                            .variant(ButtonVariant::Danger)
                            .icon(IconName::Trash)
                            .on_click(cx.listener(|this, _, _, cx| this.confirm_delete(cx)))
                            .into_any_element(),
                    ],
                }
            }
        };
        Some(dialog::modal(parts, &self.dialog_focus, cancel, window, cx))
    }
}

/// How the sidebar sums up a project.
pub fn project_summary(resources: usize, workflows: usize) -> String {
    match (resources, workflows) {
        (0, 0) => tr("No resources yet", "リソースなし").into(),
        (resources, 0) => trf!("{resources} resources", "リソース {resources} 件"),
        (resources, workflows) => trf!(
            "{resources} resources · {workflows} workflows",
            "リソース {resources} 件・ワークフロー {workflows} 件"
        ),
    }
}
