//! Menus that open from a control, for picking one of a few things such as a provider.

use atelier::{IconName, IconSource, MenuItem, Popover};
use gpui::{
    AnyElement, App, Corner, ElementId, IntoElement, MouseDownEvent, ParentElement, Styled, Window,
    anchored, deferred, div, point, prelude::*, px,
};
use maestro_core::ProviderKind;

use crate::chat::provider_logo;

/// Which menu is open in the window. One at a time: opening one closes the other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Menu {
    /// The provider a new commander runs on, over the composer.
    Commander,
    /// The provider a node is pinned to, in the inspector.
    RunsOn(maestro_core::NodeId),
}

/// `menu` floating from where this element is laid out, its `corner` there, over everything
/// else in the window. A click anywhere outside it calls `close`, and is not passed on, so the
/// control that opened the menu does not open it again.
pub fn floating(
    id: impl Into<ElementId>,
    menu: Popover,
    corner: Corner,
    close: impl Fn(&mut Window, &mut App) + 'static,
    window: &Window,
) -> AnyElement {
    let viewport = window.viewport_size();
    let backdrop = anchored().position(point(px(0.), px(0.))).child(
        div()
            .id(id)
            .w(viewport.width)
            .h(viewport.height)
            .occlude()
            .on_any_mouse_down(move |_: &MouseDownEvent, window, cx| close(window, cx)),
    );
    div()
        .relative()
        .size_0()
        .child(deferred(backdrop).with_priority(1))
        .child(
            deferred(anchored().anchor(corner).snap_to_window_with_margin(px(8.)).child(menu))
                .with_priority(2),
        )
        .into_any_element()
}

/// The mark for `provider` in a picker: its vendor's logo, or a plain glyph where there is
/// none, so every row lines up.
pub fn provider_icon(provider: ProviderKind) -> IconSource {
    match provider_logo(provider) {
        Some(logo) => logo.into(),
        None => IconName::Code.into(),
    }
}

/// A menu row naming `provider`, led by its mark.
pub fn provider_item(id: impl Into<ElementId>, provider: ProviderKind) -> MenuItem {
    MenuItem::new(id, provider.label()).icon(provider_icon(provider))
}
