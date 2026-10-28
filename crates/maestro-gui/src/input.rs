//! Editable text, drawn in Atelier's colours.
//!
//! Atelier ships no text editor, so the caret, selection and IME come from gpui-component's
//! input. Its own chrome is switched off ([`editor`]) and its colours are taken from the Atelier
//! theme ([`follow_theme`]), so it reads as the inside of an Atelier field or composer. The input
//! needs gpui-component's `Root` as the window's root view, which `main` provides.

use atelier::ActiveTheme;
use gpui::{App, AppContext, Context, Entity, Window};
use gpui_component::input::{Input, InputState};

/// Gives gpui-component the Atelier theme's text, caret and selection colours.
pub fn follow_theme(cx: &mut App) {
    let theme = cx.theme().clone();
    let colors = &theme.colors;
    let component = gpui_component::Theme::global_mut(cx);
    component.font_family = theme.typography.ui.family.clone();
    component.font_size = theme.typography.size_md;
    component.mono_font_family = theme.typography.mono.family.clone();
    component.radius = theme.shape.radius_sm;
    component.foreground = colors.text;
    component.muted_foreground = colors.text_faint;
    component.caret = colors.accent;
    component.selection = colors.accent_wash;
    component.background = gpui::transparent_black();
    component.input = colors.border;
    component.border = colors.border;
    component.ring = colors.focus_ring;
    component.accent = colors.hover;
    component.muted = colors.sunken;
}

/// A one-line field.
pub fn line<V>(
    placeholder: &'static str,
    value: &str,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Entity<InputState> {
    let value = value.to_owned();
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))
}

/// A field that grows to `max_rows` lines as text is written, then scrolls.
pub fn area<V>(
    placeholder: &'static str,
    value: &str,
    max_rows: usize,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Entity<InputState> {
    let value = value.to_owned();
    cx.new(|cx| {
        InputState::new(window, cx)
            .multi_line(true)
            .auto_grow(1, max_rows)
            .placeholder(placeholder)
            .default_value(value)
    })
}

/// The editable text alone, for an Atelier `TextField` or `Composer` to frame.
pub fn editor(state: &Entity<InputState>) -> Input {
    Input::new(state).appearance(false)
}

/// The text without the newline a send shortcut may have left at its end.
pub fn text(state: &Entity<InputState>, cx: &App) -> String {
    state.read(cx).value().trim_end_matches('\n').to_owned()
}

/// Empties a field.
pub fn clear(state: &Entity<InputState>, window: &mut Window, cx: &mut App) {
    state.update(cx, |state, cx| state.set_value("", window, cx));
}
