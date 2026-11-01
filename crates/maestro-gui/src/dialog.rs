//! Dialogs over the whole window, for a question that has to be answered before anything else:
//! a project's new name, or whether to delete it.

use atelier::{ActiveTheme, Label, TextColor, TextSize};
use gpui::{
    AnyElement, App, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseDownEvent,
    ParentElement, Styled, Window, anchored, deferred, div, point, px,
};
use maestro_core::ProjectId;

/// The width of a dialog.
const WIDTH: f32 = 400.;

/// Which dialog is open in the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialog {
    /// Asks for a project's new name.
    Rename(ProjectId),
    /// Asks whether to delete a project and its workflows.
    Delete(ProjectId),
}

impl Dialog {
    /// The project it is about.
    pub fn project(self) -> ProjectId {
        match self {
            Self::Rename(project) | Self::Delete(project) => project,
        }
    }
}

/// What a dialog shows: a title, the text under it, what it asks for, an error from the last
/// try, and its buttons, the one that commits last.
pub struct Parts {
    /// Its title.
    pub title: String,
    /// The text under the title.
    pub message: Option<String>,
    /// A field or anything else it asks for.
    pub body: Option<AnyElement>,
    /// Why the last try was refused.
    pub error: Option<String>,
    /// Its buttons, from Cancel to the one that commits.
    pub actions: Vec<AnyElement>,
}

/// `parts` in a dialog centred over the window, which dims everything under it. Escape and a
/// click outside it call `cancel`. `focus` is given to the dialog so Escape reaches it before
/// anything inside it takes focus.
pub fn modal(
    parts: Parts,
    focus: &FocusHandle,
    cancel: impl Fn(&mut Window, &mut App) + Clone + 'static,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let viewport = window.viewport_size();
    let on_escape = cancel.clone();
    let surface = div()
        .id("dialog")
        .track_focus(focus)
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" {
                on_escape(window, cx);
                cx.stop_propagation();
            }
        })
        // A press on the dialog itself is not one outside it.
        .on_any_mouse_down(|_: &MouseDownEvent, _, cx| cx.stop_propagation())
        .flex()
        .flex_col()
        .gap(theme.spacing.md)
        .w(px(WIDTH))
        .max_w(viewport.width - px(32.))
        .p(theme.spacing.lg)
        .rounded(theme.shape.radius_lg)
        .bg(theme.colors.overlay)
        .border_1()
        .border_color(theme.colors.border)
        .shadow(theme.elevation.high.clone())
        .child(Label::new(parts.title).weight(theme.typography.label_weight))
        .children(
            parts
                .message
                .map(|message| Label::new(message).size(TextSize::Small).color(TextColor::Muted)),
        )
        .children(parts.body)
        .children(
            parts
                .error
                .map(|error| Label::new(error).size(TextSize::Small).color(TextColor::Danger)),
        )
        .child(div().flex().justify_end().gap(theme.spacing.sm).children(parts.actions));
    let scrim = div()
        .id("dialog-scrim")
        .w(viewport.width)
        .h(viewport.height)
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.colors.shadow.opacity(0.32))
        .occlude()
        .on_any_mouse_down(move |_: &MouseDownEvent, window, cx| cancel(window, cx))
        .child(surface);
    deferred(anchored().position(point(px(0.), px(0.))).child(scrim))
        .with_priority(3)
        .into_any_element()
}
