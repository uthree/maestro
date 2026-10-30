//! Editable text, drawn in Atelier's colours.
//!
//! Atelier ships no text editor, so the caret, selection and IME come from gpui-component's
//! input. Its own chrome is switched off ([`editor`]) and its colours are taken from the Atelier
//! theme ([`follow_theme`]), so it reads as the inside of an Atelier field or composer. The input
//! needs gpui-component's `Root` as the window's root view, which `main` provides.

use atelier::ActiveTheme;
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, KeyBinding, ParentElement,
    Pixels, Styled, Window, div,
};
use gpui_component::input::{Enter, Input, InputState};

/// Gives gpui-component's inputs, menus and dialogs the Atelier theme's colours.
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
    component.popover = colors.overlay;
    component.popover_foreground = colors.text;
    component.list = colors.overlay;
    component.list_hover = colors.hover;
    component.list_active = colors.selected;
    component.danger = colors.danger;
    component.danger_hover = colors.danger.opacity(0.9);
    component.danger_active = colors.danger.opacity(0.8);
    component.danger_foreground = theme.text_on(colors.danger);
}

/// Makes Shift+Enter start a new line in a multi-line field, so plain Enter can send.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("shift-enter", Enter { secondary: true }, Some("Input"))]);
}

/// A message field: Enter calls `send`, and Shift+Enter (or Ctrl+Enter) starts a new line.
pub fn message(
    state: &Entity<InputState>,
    send: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // The parent sees the action first, while it is captured, and keeps the field from
    // turning a plain Enter into a newline.
    div()
        .w_full()
        .capture_action(move |action: &Enter, window, cx| {
            if !action.secondary {
                send(window, cx);
                cx.stop_propagation();
            }
        })
        .child(editor(state))
}

/// Holds a composer at `width`. Atelier's composer and the slots that hold it size to their
/// contents, so without this it shrinks to its buttons once a transcript sits above it.
pub fn stretch(width: Pixels, composer: impl IntoElement) -> impl IntoElement {
    div().flex().flex_col().w(width).child(composer)
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
