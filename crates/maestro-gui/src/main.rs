//! Maestro's desktop frontend: talk with the commander, watch and edit the workflows it plans,
//! and steer the workers on each node.
//!
//! ```text
//! maestro [--ssh DESTINATION] [--daemon PROGRAM] [--repo PATH] [--base BRANCH]
//!         [--appearance dark|light] [--direction glass|studio|mono] [--lang en|ja]
//! ```
//!
//! With `--ssh` the daemon, the repository and every agent are on that machine and only the
//! window is here, and that machine needs `maestro-daemon` installed. Without it the daemon runs
//! on this machine from this same executable (`maestro --stdio`, `maestro serve`), so it starts
//! by itself and always speaks this window's protocol.
//!
//! The window opens a project: the one with `--repo` among its folders, made if there is none,
//! or else the newest.
//!
//! The window speaks English or Japanese: `--lang`, else `MAESTRO_LANG`, else the system's
//! language.

mod app;
mod chat;
mod graph;
mod i18n;
mod input;
mod link;
mod panels;
mod project;

use std::path::PathBuf;
use std::process::ExitCode;

use atelier::{Appearance, Assets, Direction, Theme};
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};
use maestro_client::Launch;

use crate::app::{Maestro, Start};

/// The smallest window the layout fits without cutting off controls.
const MIN_SIZE: (f32, f32) = (900., 600.);

/// The value after `name` on the command line.
fn argument(name: &str) -> Option<String> {
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if arg == name {
            return args.next();
        }
    }
    None
}

/// Where the daemon is, from the command line.
fn launch() -> Launch {
    let program = argument("--daemon");
    match argument("--ssh") {
        Some(destination) => Launch::Ssh {
            destination,
            program: program.unwrap_or_else(|| maestro_client::DAEMON.into()),
        },
        None => Launch::Local { program: program.unwrap_or_else(this_program) },
    }
}

/// This executable, which carries the daemon.
fn this_program() -> String {
    std::env::current_exe()
        .map(|exe| exe.display().to_string())
        .unwrap_or_else(|_| maestro_client::DAEMON.into())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if maestro_daemon::resident::requested(&args) {
        return maestro_daemon::resident::run(&args);
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    if let Some(lang) = argument("--lang").and_then(|tag| i18n::Lang::from_tag(&tag)) {
        i18n::set(lang);
    }
    let launch = launch();
    let start = Start::new(
        argument("--repo").map(PathBuf::from),
        argument("--base").unwrap_or_else(|| "main".into()),
    );
    let direction =
        argument("--direction").and_then(|id| Direction::from_id(&id)).unwrap_or_default();
    let appearance = match argument("--appearance").as_deref() {
        Some("light") => Appearance::Light,
        _ => Appearance::Dark,
    };

    Application::new().with_assets(Assets::default()).run(move |cx: &mut App| {
        let theme = Theme::new(direction, appearance);
        let background = theme.window_background();
        atelier::init(cx, theme);
        // The words Atelier's components draw themselves: tooltips, Send, Allow, states.
        if i18n::lang() == i18n::Lang::Ja {
            atelier::Strings::japanese().install(cx);
        }
        gpui_component::init(cx);
        input::follow_theme(cx);
        input::bind_keys(cx);

        let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);
        let options = atelier::custom_frame(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
                window_background: background,
                titlebar: Some(TitlebarOptions {
                    title: Some("Maestro".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &Theme::new(direction, appearance),
        );
        cx.open_window(options, |window, cx| {
            atelier::set_window_background(window, &Theme::new(direction, appearance));
            let view = cx.new(|cx| Maestro::new(launch, start, window, cx));
            // gpui-component's text input finds its window through a `Root` at the top. Its
            // own frame only draws for client-side decorations, which Atelier asks for on
            // Linux alone; on macOS and Windows it is a plain container.
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        })
        .expect("failed to open the window");
        cx.activate(true);
    });
    ExitCode::SUCCESS
}
