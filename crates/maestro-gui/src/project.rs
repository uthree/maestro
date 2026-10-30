//! The open project's resources, in the pane beside the conversation: its name, the
//! repositories, folders and machines its commander may choose from, and the AI providers its
//! work may run on.

use atelier::{
    ActiveTheme, Button, ButtonVariant, ControlSize, FormField, FormGroup, IconName, Label,
    ListItem, SectionHeader, Segment, SegmentedControl, Switch, TextColor, TextField, TextSize,
};
use gpui::{Context, IntoElement, ParentElement, Pixels, Styled, div, prelude::*};
use maestro_core::{Host, ProviderKind, Resource};

use crate::app::Maestro;
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
        let project_id = project.id;
        let workflows = self.mirror.workflows_of(project.id).count();

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
                    .child(TextField::new().child(input::editor(&self.fields.resource))),
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
                                move |this, _, window, cx| {
                                    this.delete_project(project_id, window, cx);
                                },
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
