//! The settings window: the window's language and look, the coding-agent CLIs the daemon may
//! hand work to (found, installed and updated from here), and when work is compared across
//! models.
//!
//! It follows Atelier's preferences layout: pages in a sidebar, each page a header and groups of
//! labelled fields in a readable column, and a status bar reporting what the last action did.
//! Switches and pickers that stand alone apply at once; the comparison settings take effect
//! together, so that page has Save and Revert at its foot.
//!
//! The language and the look belong to this computer and are saved here ([`Preferences`]).
//! Everything else is the daemon's, wherever it runs: this window reads it from the main
//! window's [`Mirror`](maestro_client::Mirror) and changes it with commands, so a TUI on the same
//! daemon sees the same providers and the same settings.

use std::sync::Arc;

use atelier::{
    ActiveTheme, Appearance, Badge, Button, ControlSize, Direction, Edge, FormField, FormGroup,
    IconName, Label, ListItem, SectionHeader, Segment, SegmentedControl, Sidebar, Spinner,
    StatusBar, Switch, TextColor, TextSize, Theme, TitleBar, Tone, WindowFrame, agent::CodeBlock,
};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Window, div, prelude::*, px,
};
use maestro_client::{Client, ClientError};
use maestro_core::{
    Comparison, Difficulty, Host, ProviderKind, ProviderStatus, Router, SetupAction,
};
use maestro_protocol::{Command, Reply};

use crate::app::{Connection, Maestro};
use crate::chat::provider_logo;
use crate::i18n::{self, Lang, tr};
use crate::prefs::Preferences;
use crate::{input, link, trf};

/// The settings window's size, and the smallest it may be.
const SIZE: (f32, f32) = (1040., 760.);
const MIN_SIZE: (f32, f32) = (760., 520.);
/// How many lines of an install's output stay on screen.
const OUTPUT_LINES: usize = 12;

/// Opens the settings window, or brings it forward when it is already open.
pub fn open(app: &mut Maestro, cx: &mut Context<Maestro>) {
    if let Some(handle) = app.settings
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    let main = cx.entity();
    // The window draws as it opens, and reads the main window then, so it opens once the main
    // window is no longer being updated.
    cx.defer(move |cx| {
        let options = crate::window_options("Settings", SIZE, MIN_SIZE, cx);
        let opened = cx.open_window(options, |window, cx| {
            atelier::set_window_background(window, &cx.theme().clone());
            let view = cx.new(|cx| Settings::new(main.clone(), cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        });
        main.update(cx, |app, cx| {
            match opened {
                Ok(handle) => app.settings = Some(handle.into()),
                Err(error) => app.error = Some(error.to_string()),
            }
            cx.notify();
        });
    });
}

/// One page of the window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    General,
    Providers,
    Routing,
}

impl Page {
    const ALL: [Self; 3] = [Self::General, Self::Providers, Self::Routing];

    fn title(self) -> &'static str {
        match self {
            Self::General => tr("General", "一般"),
            Self::Providers => tr("Agent providers", "AIエージェント"),
            Self::Routing => tr("Work allocation", "作業の割り当て"),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::General => tr("Language and appearance", "表示言語と外観"),
            Self::Providers => tr(
                "The coding-agent CLIs work can go to: install, update, switch off",
                "作業を任せるCLIの導入・更新・利用の切り替え",
            ),
            Self::Routing => tr(
                "When a step runs on several models to compare them",
                "同じステップを複数のモデルで比較する条件",
            ),
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::General => IconName::Settings,
            Self::Providers => IconName::Sparkles,
            Self::Routing => IconName::Sliders,
        }
    }
}

/// The settings window's view.
pub struct Settings {
    /// The main window, which holds the connection and the copy of the daemon's state.
    main: Entity<Maestro>,
    page: Page,
    /// The comparison settings being edited, until saved or reverted.
    draft: Option<Comparison>,
    /// Whether the daemon is looking for the CLIs.
    scanning: bool,
    /// What the last action did.
    status: Option<(SharedString, Tone)>,
    preferences: Preferences,
    _follow: Subscription,
}

impl Settings {
    fn new(main: Entity<Maestro>, cx: &mut Context<Self>) -> Self {
        // The main window's copy moves with the daemon's events; this window redraws with it.
        let follow = cx.observe(&main, |_, _, cx| cx.notify());
        let mut settings = Self {
            main,
            page: Page::General,
            draft: None,
            scanning: false,
            status: None,
            preferences: Preferences::load(),
            _follow: follow,
        };
        settings.scan(cx);
        settings
    }

    /// The daemon's machine, where the CLIs run. Over SSH that is the server, not this computer.
    fn host() -> Host {
        Host::Local
    }

    fn client(&self, cx: &App) -> Option<Arc<Client>> {
        match &self.main.read(cx).connection {
            Connection::Ready(client) => Some(client.clone()),
            _ => None,
        }
    }

    /// Sends a command from a background task and hands the result to `then`.
    fn request(
        &mut self,
        command: Command,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Result<Reply, ClientError>, &mut Context<Self>) + 'static,
    ) {
        let Some(client) = self.client(cx) else {
            self.report(
                tr("Not connected to the daemon.", "デーモンに接続していません。"),
                Tone::Danger,
            );
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let result =
                cx.background_executor().spawn(async move { client.request(command) }).await;
            this.update(cx, |this, cx| {
                then(this, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn report(&mut self, message: impl Into<SharedString>, tone: Tone) {
        self.status = Some((message.into(), tone));
    }

    /// Asks the daemon which CLIs its machine has.
    fn scan(&mut self, cx: &mut Context<Self>) {
        if self.client(cx).is_none() {
            return;
        }
        self.scanning = true;
        let host = Self::host();
        self.request(Command::ListProviders { host: host.clone() }, cx, move |this, result, cx| {
            this.scanning = false;
            match result {
                Ok(Reply::Providers { providers }) => {
                    this.main.update(cx, |main, cx| {
                        main.mirror.set_providers(&host, providers);
                        cx.notify();
                    });
                }
                Ok(_) => {}
                Err(error) => this.report(error.to_string(), Tone::Danger),
            }
        });
    }

    fn setup(&mut self, provider: ProviderKind, action: SetupAction, cx: &mut Context<Self>) {
        let command = Command::SetupProvider { host: Self::host(), provider, action };
        self.request(command, cx, move |this, result, _| {
            if let Err(error) = result {
                this.report(error.to_string(), Tone::Danger);
            }
        });
    }

    /// The daemon's router settings, as the main window last heard them.
    fn router(&self, cx: &App) -> Router {
        self.main.read(cx).mirror.router.clone()
    }

    fn save_router(&mut self, router: Router, saved: &'static str, cx: &mut Context<Self>) {
        self.request(Command::SetRouter { router }, cx, move |this, result, _| match result {
            Ok(_) => {
                this.draft = None;
                this.report(saved, Tone::Success);
            }
            Err(error) => this.report(error.to_string(), Tone::Danger),
        });
    }

    fn set_enabled(&mut self, provider: ProviderKind, enabled: bool, cx: &mut Context<Self>) {
        let mut router = self.router(cx);
        if enabled {
            router.disabled.remove(&provider);
        } else {
            router.disabled.insert(provider);
        }
        // Only this changes; a comparison draft stays a draft.
        let draft = self.draft.take();
        let saved = if enabled {
            tr("Work can go to this provider again", "このプロバイダーに作業を任せます")
        } else {
            tr("No new work goes to this provider", "このプロバイダーには新しい作業を任せません")
        };
        self.request(Command::SetRouter { router }, cx, move |this, result, _| {
            this.draft = draft;
            match result {
                Ok(_) => this.report(saved, Tone::Success),
                Err(error) => this.report(error.to_string(), Tone::Danger),
            }
        });
    }

    // The window's own choices.

    fn set_lang(&mut self, lang: Option<Lang>, cx: &mut Context<Self>) {
        self.preferences.lang = lang.map(|lang| lang.tag().into());
        i18n::set(lang.unwrap_or_else(i18n::detect));
        let strings = match i18n::lang() {
            Lang::En => atelier::Strings::english(),
            Lang::Ja => atelier::Strings::japanese(),
        };
        strings.install(cx);
        self.save_preferences();
    }

    fn set_theme(&mut self, direction: Direction, appearance: Appearance, cx: &mut Context<Self>) {
        self.preferences.set_direction(direction);
        self.preferences.set_appearance(appearance);
        let theme = Theme::new(direction, appearance);
        theme.clone().install(cx);
        input::follow_theme(cx);
        // Every window's material follows, once this one is no longer being updated.
        cx.defer(move |cx| {
            for window in cx.windows() {
                window
                    .update(cx, |_, window, _| atelier::set_window_background(window, &theme))
                    .ok();
            }
            cx.refresh_windows();
        });
        self.save_preferences();
    }

    fn save_preferences(&mut self) {
        match self.preferences.save() {
            Ok(()) => self.report(
                tr("Saved on this computer", "このコンピューターに保存しました"),
                Tone::Success,
            ),
            Err(error) => self.report(
                trf!("Could not save: {error}", "保存できませんでした: {error}"),
                Tone::Danger,
            ),
        }
    }

    // Drawing.

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let pages = Page::ALL.map(|page| {
            ListItem::new(page.title(), page.title())
                .icon(page.icon())
                .description(page.description())
                .description_lines(2)
                .selected(self.page == page)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.page = page;
                    this.status = None;
                    cx.notify();
                }))
        });
        Sidebar::new(Edge::Left)
            .id("pages")
            .overflow_y_scroll()
            .w(px(260.))
            .p(theme.spacing.sm)
            .gap(theme.spacing.xxs)
            .children(pages)
    }

    fn general(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let lang = match self.preferences.lang() {
            None => 0,
            Some(Lang::En) => 1,
            Some(Lang::Ja) => 2,
        };
        let direction = theme.direction;
        let appearance = theme.appearance;
        let direction_index =
            Direction::ALL.iter().position(|candidate| *candidate == direction).unwrap_or(0);
        let place = link::describe(&self.main.read(cx).launch);
        vec![
            FormGroup::new(tr("Language", "言語"))
                .child(
                    FormField::new(tr("Display language", "表示言語"))
                        .description(tr(
                            "The language of Maestro's own words. What the agents write is shown \
                             as they write it.",
                            "Maestro 自身の表示に使う言語です。エージェントの返答は書かれたままの\
                             言語で表示します。",
                        ))
                        .child(
                            SegmentedControl::new(
                                "language",
                                [
                                    Segment::label(tr("System", "システムに合わせる")),
                                    Segment::label("English"),
                                    Segment::label("日本語"),
                                ],
                                lang,
                            )
                            .size(ControlSize::Small)
                            .on_select(cx.listener(|this, index: &usize, _, cx| {
                                let lang = match index {
                                    1 => Some(Lang::En),
                                    2 => Some(Lang::Ja),
                                    _ => None,
                                };
                                this.set_lang(lang, cx);
                                cx.notify();
                            })),
                        ),
                )
                .into_any_element(),
            FormGroup::new(tr("Appearance", "外観"))
                .description(tr(
                    "Saved on this computer and applied at once, to every Maestro window.",
                    "このコンピューターに保存され、すべての Maestro のウィンドウにすぐ反映されます。",
                ))
                .child(
                    FormField::new(tr("Mode", "モード")).child(
                        SegmentedControl::new(
                            "appearance",
                            [Segment::label(tr("Dark", "ダーク")), Segment::label(tr("Light", "ライト"))],
                            if appearance.is_dark() { 0 } else { 1 },
                        )
                        .size(ControlSize::Small)
                        .on_select(cx.listener(move |this, index: &usize, _, cx| {
                            let appearance =
                                if *index == 0 { Appearance::Dark } else { Appearance::Light };
                            this.set_theme(direction, appearance, cx);
                        })),
                    ),
                )
                .child(
                    FormField::new(tr("Style", "スタイル"))
                        .description(tr(
                            "Glass lets the desktop show through the chrome; Studio and Mono are \
                             solid.",
                            "Glass は枠の部分にデスクトップが透けます。Studio と Mono は不透明です。",
                        ))
                        .child(
                            SegmentedControl::new(
                                "direction",
                                Direction::ALL.map(|direction| Segment::label(direction.label())),
                                direction_index,
                            )
                            .size(ControlSize::Small)
                            .on_select(cx.listener(move |this, index: &usize, _, cx| {
                                this.set_theme(Direction::ALL[*index], appearance, cx);
                            })),
                        ),
                )
                .into_any_element(),
            FormGroup::new(tr("Connection", "接続"))
                .child(
                    FormField::new(tr("Daemon", "デーモン"))
                        .description(tr(
                            "Where the agents run and the work is kept. Choose it when starting \
                             Maestro, with --ssh.",
                            "エージェントが動き、作業が保存される場所です。起動時に --ssh で\
                             選びます。",
                        ))
                        .child(Label::new(place).color(TextColor::Muted)),
                )
                .into_any_element(),
        ]
    }

    fn providers(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let host = Self::host();
        let router = self.router(cx);
        let place = link::describe(&self.main.read(cx).launch);
        ProviderKind::ALL
            .into_iter()
            .map(|provider| {
                let main = self.main.read(cx);
                let status = main.mirror.provider(&host, provider).cloned();
                let setup = main.mirror.setup(&host, provider).cloned();
                self.provider(
                    provider,
                    status,
                    setup,
                    !router.disabled.contains(&provider),
                    &place,
                    cx,
                )
            })
            .collect()
    }

    fn provider(
        &self,
        provider: ProviderKind,
        status: Option<ProviderStatus>,
        setup: Option<maestro_client::mirror::Setup>,
        enabled: bool,
        place: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let installed = status.as_ref().is_some_and(ProviderStatus::installed);
        let running = status.as_ref().and_then(|status| status.setup).is_some()
            || setup.as_ref().is_some_and(|setup| setup.finished.is_none());
        let id = provider.executable();

        let badge = match &status {
            _ if running => Badge::new(tr("Running…", "実行中…")).tone(Tone::Accent),
            None => Badge::new(tr("Looking…", "確認中…")).tone(Tone::Neutral),
            Some(status) if status.installed() => Badge::new(
                status
                    .version
                    .clone()
                    .unwrap_or_else(|| tr("Installed", "インストール済み").into()),
            )
            .tone(Tone::Success)
            .icon(IconName::Check),
            Some(_) => Badge::new(tr("Not installed", "未インストール")).tone(Tone::Neutral),
        };
        let path = status.as_ref().and_then(|status| status.path.clone());
        let summary = div()
            .flex()
            .items_center()
            .gap(theme.spacing.sm)
            .min_w_0()
            .children(provider_logo(provider).map(|logo| atelier::Icon::new(logo).size(px(18.))))
            .child(badge)
            .children(path.map(|path| {
                div().min_w_0().child(
                    Label::new(path).size(TextSize::Small).color(TextColor::Muted).truncate(),
                )
            }));

        let action = if installed { SetupAction::Update } else { SetupAction::Install };
        let script = provider.setup_script(action);
        let button = Button::new(
            SharedString::from(format!("setup-{id}")),
            if installed { tr("Update", "更新") } else { tr("Install", "インストール") },
        )
        .icon(if installed { IconName::Refresh } else { IconName::Plus })
        .when(!installed, Button::primary)
        .disabled(running || status.is_none())
        .on_click(cx.listener(move |this, _, _, cx| this.setup(provider, action, cx)));

        let mut group = FormGroup::new(provider.label())
            .description(trf!(
                "Runs as the {} command on {place}.",
                "{place}で {} コマンドとして動きます。",
                provider.executable()
            ))
            .child(summary)
            .child(
                FormField::new(tr("Use for work", "作業に使う"))
                    .description(if installed {
                        tr(
                            "The commander and the router may give steps to it.",
                            "ルーターがこのプロバイダーにステップを割り当てられます。",
                        )
                    } else {
                        tr(
                            "Takes effect once it is installed.",
                            "インストールすると有効になります。",
                        )
                    })
                    .beside()
                    .child(Switch::new(SharedString::from(format!("use-{id}")), enabled).on_click(
                        cx.listener(move |this, _, _, cx| this.set_enabled(provider, !enabled, cx)),
                    )),
            )
            .child(
                FormField::new(if installed {
                    tr("Update to the latest release", "最新版に更新")
                } else {
                    tr("Install", "インストール")
                })
                .description(trf!(
                    "Runs this on {place}, as the project documents it.",
                    "公式の手順どおり、{place}でこのコマンドを実行します。"
                ))
                .child(
                    CodeBlock::new(SharedString::from(format!("script-{id}")), script).copyable(),
                )
                .child(button),
            );
        if let Some(setup) = setup {
            group = group.child(self.output(provider, &setup, status.as_ref(), cx));
        }
        if installed {
            group = group.child(
                FormField::new(tr("Sign in", "サインイン"))
                    .description(trf!(
                        "Each CLI signs in to your account in a terminal. Run this once on \
                         {place} if it has not been signed in.",
                        "サインインは各CLIがターミナルで行います。まだなら{place}で一度実行して\
                         ください。"
                    ))
                    .child(
                        CodeBlock::new(
                            SharedString::from(format!("sign-in-{id}")),
                            provider.sign_in(),
                        )
                        .copyable(),
                    ),
            );
        }
        group.into_any_element()
    }

    /// What an install or update wrote, and how it ended.
    fn output(
        &self,
        provider: ProviderKind,
        setup: &maestro_client::mirror::Setup,
        status: Option<&ProviderStatus>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let start = setup.lines.len().saturating_sub(OUTPUT_LINES);
        let text = setup.lines[start..].join("\n");
        let text = if text.is_empty() { "…".to_string() } else { text };
        let verb = match setup.action {
            SetupAction::Install => tr("Install", "インストール"),
            SetupAction::Update => tr("Update", "更新"),
        };
        let (line, tone) = match setup.finished {
            None => (trf!("{verb} running…", "{verb}を実行中…"), Tone::Accent),
            Some(true) if !status.is_some_and(ProviderStatus::installed) => (
                trf!(
                    "{verb} finished, but {} is not on the daemon's PATH. Add its folder to \
                     PATH and restart the daemon.",
                    "{verb}は終わりましたが、デーモンの PATH に {} がありません。インストール先を \
                     PATH に加えて、デーモンを再起動してください。",
                    provider.executable()
                ),
                Tone::Warning,
            ),
            Some(true) => (trf!("{verb} finished", "{verb}が完了しました"), Tone::Success),
            Some(false) => (
                trf!(
                    "{verb} failed; see the output",
                    "{verb}に失敗しました。出力を確認してください"
                ),
                Tone::Danger,
            ),
        };
        let field = FormField::new(tr("Output", "出力")).child(
            div()
                .flex()
                .flex_col()
                .gap(theme.spacing.xs)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme.spacing.sm)
                        .when(setup.finished.is_none(), |row| {
                            row.child(Spinner::new(SharedString::from(format!(
                                "setup-spinner-{}",
                                provider.executable()
                            ))))
                        })
                        .child(Label::new(line.clone()).size(TextSize::Small).color(match tone {
                            Tone::Success => TextColor::Success,
                            Tone::Warning => TextColor::Warning,
                            Tone::Danger => TextColor::Danger,
                            _ => TextColor::Muted,
                        })),
                )
                .child(
                    CodeBlock::new(
                        SharedString::from(format!("output-{}", provider.executable())),
                        text,
                    )
                    .title(setup.script.clone())
                    .terminal(),
                ),
        );
        field.into_any_element()
    }

    fn routing(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let router = self.router(cx);
        let comparison = self.draft.clone().unwrap_or_else(|| router.comparison.clone());
        let edit = |change: fn(&mut Comparison, usize)| {
            cx.listener(move |this: &mut Self, index: &usize, _, cx| {
                let mut draft =
                    this.draft.clone().unwrap_or_else(|| this.router(cx).comparison.clone());
                change(&mut draft, *index);
                this.draft = Some(draft);
                this.status = None;
                cx.notify();
            })
        };

        let contenders = [1, 2, 3, 4];
        let (contender_values, contender_index) = choices(&contenders, comparison.contenders);
        let difficulty_index = Difficulty::ALL
            .iter()
            .position(|difficulty| *difficulty == comparison.min_difficulty)
            .unwrap_or(0);
        let (worker_values, worker_index) = choices(&[1, 2, 3, 4, 6, 8], comparison.max_workers);

        let mut limits = FormGroup::new(tr("Limits", "同時実行の上限")).description(tr(
            "Comparing costs compute and usage. Past these limits steps run on one model; work \
             that is not compared still starts.",
            "比較は計算資源と利用枠を使います。上限を超えると比較せず1つのモデルで実行します。\
             比較しない作業はそのまま始まります。",
        ));
        limits = limits.child(
            FormField::new(tr("Workers on the host", "ホストのワーカー数"))
                .description(tr(
                    "Steps are compared only while fewer workers than this are running.",
                    "動いているワーカーがこの数より少ないときだけ比較します。",
                ))
                .child(
                    SegmentedControl::new(
                        "max-workers",
                        worker_values.iter().map(|count| Segment::label(count.to_string())),
                        worker_index,
                    )
                    .size(ControlSize::Small)
                    .on_select(edit(|draft, index| {
                        let (values, _) = choices(&[1, 2, 3, 4, 6, 8], draft.max_workers);
                        draft.max_workers = values[index];
                    })),
                ),
        );
        for provider in ProviderKind::ALL {
            let limit = comparison.per_provider.get(&provider).copied().unwrap_or(0);
            let (values, index) = choices(&[0, 1, 2, 3, 4], limit);
            limits = limits.child(
                FormField::new(trf!("{} workers", "{} のワーカー数", provider.label())).child(
                    SegmentedControl::new(
                        SharedString::from(format!("limit-{}", provider.executable())),
                        values.iter().map(|&count| {
                            Segment::label(if count == 0 {
                                tr("No limit", "上限なし").to_string()
                            } else {
                                count.to_string()
                            })
                        }),
                        index,
                    )
                    .size(ControlSize::Small)
                    .on_select(cx.listener(
                        move |this, index: &usize, _, cx| {
                            let mut draft = this
                                .draft
                                .clone()
                                .unwrap_or_else(|| this.router(cx).comparison.clone());
                            let current = draft.per_provider.get(&provider).copied().unwrap_or(0);
                            let (values, _) = choices(&[0, 1, 2, 3, 4], current);
                            match values[*index] {
                                0 => draft.per_provider.remove(&provider),
                                count => draft.per_provider.insert(provider, count),
                            };
                            this.draft = Some(draft);
                            this.status = None;
                            cx.notify();
                        },
                    )),
                ),
            );
        }

        let models = router.catalog.models.iter().map(|profile| {
            let model = if profile.model.is_empty() {
                tr("CLI default", "CLIの既定").to_string()
            } else {
                profile.model.clone()
            };
            let grade = match profile.grade {
                maestro_core::ModelGrade::Light => tr("Light", "軽量"),
                maestro_core::ModelGrade::Standard => tr("Standard", "標準"),
                maestro_core::ModelGrade::Frontier => tr("Frontier", "最上位"),
            };
            Label::new(format!("{} · {model} · {grade}", profile.provider.label()))
                .size(TextSize::Small)
                .color(TextColor::Muted)
        });

        vec![
            FormGroup::new(tr("Comparison", "比較"))
                .description(tr(
                    "When there is room, a step runs on several models at once and you keep the \
                     better result. Your picks teach the router which model suits which work.",
                    "余裕があるとき、同じステップを複数のモデルで同時に走らせ、良い方を選べます。\
                     選んだ結果から、どのモデルがどの作業に向くかを学びます。",
                ))
                .child(
                    FormField::new(tr("Models per step", "1ステップで比べるモデル数")).child(
                        SegmentedControl::new(
                            "contenders",
                            contender_values.iter().map(|&count| {
                                Segment::label(if count == 1 {
                                    tr("Off", "比較しない").to_string()
                                } else {
                                    count.to_string()
                                })
                            }),
                            contender_index,
                        )
                        .size(ControlSize::Small)
                        .on_select(edit(|draft, index| {
                            let (values, _) = choices(&[1, 2, 3, 4], draft.contenders);
                            draft.contenders = values[index];
                        })),
                    ),
                )
                .child(
                    FormField::new(tr("From difficulty", "比較する難易度"))
                        .description(tr(
                            "Easier steps always run on one model.",
                            "これより簡単なステップは、常に1つのモデルで実行します。",
                        ))
                        .child(
                            SegmentedControl::new(
                                "min-difficulty",
                                Difficulty::ALL
                                    .map(|difficulty| Segment::label(i18n::difficulty(difficulty))),
                                difficulty_index,
                            )
                            .size(ControlSize::Small)
                            .on_select(edit(|draft, index| {
                                draft.min_difficulty = Difficulty::ALL[index];
                            })),
                        ),
                )
                .into_any_element(),
            limits.into_any_element(),
            FormGroup::new(tr("Models", "モデル"))
                .description(tr(
                    "The models the router chooses from, their grades and the benchmark results \
                     it trusts are edited in router.json in the daemon's ~/.maestro.",
                    "ルーターが選ぶモデル、グレード、参考にするベンチマークは、デーモン側の \
                     ~/.maestro/router.json で編集します。",
                ))
                .child(div().flex().flex_col().gap(px(4.)).children(models))
                .into_any_element(),
        ]
    }

    /// Reset, Revert and Save, for the comparison page.
    fn routing_actions(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let changed = self
            .draft
            .as_ref()
            .is_some_and(|draft| *draft != self.main.read(cx).mirror.router.comparison);
        div()
            .flex()
            .items_center()
            .gap(theme.spacing.sm)
            .pt(theme.spacing.lg)
            .child(
                Button::new("defaults", tr("Reset to defaults", "初期設定に戻す"))
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.draft = Some(Comparison::default());
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(Button::new("revert", tr("Revert", "元に戻す")).disabled(!changed).on_click(
                cx.listener(|this, _, _, cx| {
                    this.draft = None;
                    cx.notify();
                }),
            ))
            .child(Button::new("save", tr("Save", "保存")).primary().disabled(!changed).on_click(
                cx.listener(|this, _, _, cx| {
                    let Some(draft) = this.draft.clone() else { return };
                    let mut router = this.router(cx);
                    router.comparison = draft;
                    this.save_router(
                        router,
                        tr("Saved to the daemon", "デーモンに保存しました"),
                        cx,
                    );
                }),
            ))
    }
}

/// The values a picker offers, with `current` among them even when it is not one of `common`,
/// and the index of `current`.
fn choices(common: &[usize], current: usize) -> (Vec<usize>, usize) {
    let mut values = common.to_vec();
    if !values.contains(&current) {
        values.push(current);
        values.sort_unstable();
    }
    let index = values.iter().position(|value| *value == current).unwrap_or(0);
    (values, index)
}

impl Render for Settings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let groups = match self.page {
            Page::General => self.general(cx),
            Page::Providers => self.providers(cx),
            Page::Routing => self.routing(cx),
        };
        let header = SectionHeader::new(self.page.title())
            .description(self.page.description())
            .when(self.page == Page::Providers, |header| {
                header.action(
                    Button::new("rescan", tr("Check again", "再確認"))
                        .icon(IconName::Refresh)
                        .size(ControlSize::Small)
                        .disabled(self.scanning)
                        .on_click(cx.listener(|this, _, _, cx| this.scan(cx))),
                )
            });
        let actions = (self.page == Page::Routing).then(|| self.routing_actions(cx));
        // Each page has its own scroll position, so a page always opens at its top.
        let page = div()
            .id(("page", self.page as usize))
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .bg(theme.colors.surface)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.xl)
                    .w_full()
                    .max_w(theme.density.form_width)
                    .px(theme.spacing.xxl)
                    .py(theme.spacing.xl)
                    .child(header)
                    .children(groups)
                    .children(actions),
            );

        let changed = self.draft.is_some();
        let status = match &self.status {
            Some((message, tone)) => StatusBar::new().message(message.clone()).tone(*tone),
            None if self.scanning => StatusBar::new()
                .message(tr("Looking for the CLIs…", "CLIを確認しています…"))
                .busy(true),
            None if changed && self.page == Page::Routing => StatusBar::new()
                .message(tr("Unsaved changes", "保存していない変更があります"))
                .tone(Tone::Warning),
            None => StatusBar::new(),
        };
        WindowFrame::new()
            .font(theme.typography.ui.clone())
            .text_color(theme.colors.text)
            .size_full()
            .flex()
            .flex_col()
            .child(TitleBar::new().title(tr("Settings", "設定")))
            .child(div().flex().flex_1().min_h_0().child(self.sidebar(cx)).child(page))
            .child(
                status.trailing(
                    Label::new(concat!("v", env!("CARGO_PKG_VERSION")))
                        .size(TextSize::XSmall)
                        .color(TextColor::Faint),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::choices;

    #[test]
    fn a_value_off_the_list_is_offered_too() {
        assert_eq!(choices(&[1, 2, 4], 2), (vec![1, 2, 4], 1));
        assert_eq!(choices(&[1, 2, 4], 3), (vec![1, 2, 3, 4], 2));
        assert_eq!(choices(&[1, 2, 4], 9), (vec![1, 2, 4, 9], 3));
    }
}
