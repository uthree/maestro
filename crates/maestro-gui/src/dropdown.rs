//! Menus that open from a control, for picking one of a few things such as a provider, and
//! context menus that open where the pointer was right-clicked.

use atelier::{IconName, IconSource, Label, MenuItem, Popover};
use gpui::{
    AnyElement, App, Corner, ElementId, IntoElement, MouseDownEvent, ParentElement, Pixels, Point,
    Styled, Window, anchored, deferred, div, point, prelude::*, px,
};
use maestro_core::{Catalog, ModelChoice, ModelGrade, ProjectId, ProviderKind};

use crate::chat::provider_logo;
use crate::i18n::tr;

/// Which menu is open in the window. One at a time: opening one closes the other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Menu {
    /// The provider a new commander runs on, over the composer.
    Commander,
    /// The provider a node is pinned to, in the inspector.
    RunsOn(maestro_core::NodeId),
    /// What can be done to a project, opened by right-clicking it in the sidebar where the
    /// pointer was, in window coordinates.
    Project(ProjectId, Point<Pixels>),
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
    div()
        .relative()
        .size_0()
        .child(backdrop(id, close, window))
        .child(
            deferred(anchored().anchor(corner).snap_to_window_with_margin(px(8.)).child(menu))
                .with_priority(2),
        )
        .into_any_element()
}

/// A context menu: `menu` with its top left corner at `position` in the window, moved in from
/// the window's edges when it would not fit. A click outside it calls `close`, as for
/// [`floating`].
pub fn at_pointer(
    id: impl Into<ElementId>,
    menu: Popover,
    position: Point<Pixels>,
    close: impl Fn(&mut Window, &mut App) + 'static,
    window: &Window,
) -> AnyElement {
    div()
        .size_0()
        .child(backdrop(id, close, window))
        .child(
            deferred(
                anchored()
                    .position(position)
                    .anchor(Corner::TopLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(menu),
            )
            .with_priority(2),
        )
        .into_any_element()
}

/// The whole window under an open menu, taking the click that closes it.
fn backdrop(
    id: impl Into<ElementId>,
    close: impl Fn(&mut Window, &mut App) + 'static,
    window: &Window,
) -> impl IntoElement {
    let viewport = window.viewport_size();
    let backdrop = anchored().position(point(px(0.), px(0.))).child(
        div()
            .id(id)
            .w(viewport.width)
            .h(viewport.height)
            .occlude()
            .on_any_mouse_down(move |_: &MouseDownEvent, window, cx| close(window, cx)),
    );
    deferred(backdrop).with_priority(1)
}

/// The mark for `provider` in a picker: its vendor's logo, or a plain glyph where there is
/// none, so every row lines up.
pub fn provider_icon(provider: ProviderKind) -> IconSource {
    match provider_logo(provider) {
        Some(logo) => logo.into(),
        None => IconName::Code.into(),
    }
}

/// A model a menu offers: which CLI runs it, its name (empty for the CLI's default) and grade.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    /// The CLI.
    pub provider: ProviderKind,
    /// The model's name; empty for the CLI's own default.
    pub model: String,
    /// Its grade.
    pub grade: ModelGrade,
}

impl Offer {
    /// Whether `choice` names this model.
    pub fn is(&self, choice: &ModelChoice) -> bool {
        choice.provider == self.provider && choice.model == self.model
    }
}

/// The models offered for each of `providers`, in order, grouped by provider.
pub fn offers(
    catalog: &Catalog,
    providers: impl IntoIterator<Item = ProviderKind>,
) -> Vec<(ProviderKind, Vec<Offer>)> {
    providers
        .into_iter()
        .map(|provider| {
            let offers = catalog
                .offered(provider)
                .into_iter()
                .map(|(model, grade)| Offer { provider, model, grade })
                .collect();
            (provider, offers)
        })
        .collect()
}

/// The heading over one provider's models in a menu.
pub fn provider_caption(provider: ProviderKind, first: bool) -> impl IntoElement + use<> {
    div()
        .px(px(8.))
        .pt(if first { px(4.) } else { px(8.) })
        .pb(px(2.))
        .child(Label::caption(provider.label()))
}

/// A menu row for one offered model, led by its provider's mark.
pub fn offer_item(id: impl Into<ElementId>, offer: &Offer) -> MenuItem {
    let label = if offer.model.is_empty() {
        tr("CLI default", "CLIの既定").to_string()
    } else {
        offer.model.clone()
    };
    MenuItem::new(id, label).icon(provider_icon(offer.provider))
}

/// How a chosen model reads on the control that opened its menu.
pub fn choice_label(choice: &ModelChoice) -> String {
    if choice.model.is_empty() {
        choice.provider.label().to_string()
    } else {
        format!("{} · {}", choice.provider.label(), choice.model)
    }
}
