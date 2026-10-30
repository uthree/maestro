//! The composer's model and reasoning controls, grouped in one anchored popover.

use std::cell::Cell;
use std::rc::Rc;

use atelier::{
    ActiveTheme, Button, ControlSize, Icon, IconName, Label, ListItem, Popover, TextColor, TextSize,
};
use gpui::{
    App, Bounds, ClickEvent, Context, Corner, Focusable, IntoElement, MouseButton, ParentElement,
    Pixels, RenderOnce, Styled, Window, canvas, div, prelude::*, relative,
};
use gpui_component::{Selectable, popover::Popover as AnchoredPopover};
use maestro_core::{Effort, ModelChoice, ModelGrade, ProviderKind};

use crate::app::Maestro;
use crate::chat::provider_logo;
use crate::i18n::tr;
use crate::trf;

const EFFORTS: [Effort; 4] = [Effort::Low, Effort::Medium, Effort::High, Effort::Max];

fn effort_label(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => tr("Low", "低"),
        Effort::Medium => tr("Medium", "中"),
        Effort::High => tr("High", "高"),
        Effort::Max => tr("Maximum", "最大"),
    }
}

fn model_label(choice: &ModelChoice) -> &str {
    if choice.model.is_empty() { choice.provider.label() } else { &choice.model }
}

/// Keeps a provider-specific model from leaking into another provider's request.
pub(crate) fn allowed_choice(choice: &ModelChoice, allowed: &[ProviderKind]) -> ModelChoice {
    if allowed.contains(&choice.provider) {
        return choice.clone();
    }
    ModelChoice {
        provider: allowed.first().copied().unwrap_or(ProviderKind::ClaudeCode),
        model: String::new(),
        grade: ModelGrade::Standard,
        effort: choice.effort,
    }
}

/// Adapts an Atelier button to gpui-component's popover selection contract.
#[derive(IntoElement)]
struct Trigger {
    button: Button,
    selected: bool,
}

impl Selectable for Trigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for Trigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.button.selected(self.selected)
    }
}

impl Maestro {
    pub(crate) fn composer_settings(
        &self,
        choice: &ModelChoice,
        editable: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let label = format!("{} · {}", model_label(choice), effort_label(choice.effort));
        let button = Button::new("composer-settings-trigger", label)
            .ghost()
            .size(ControlSize::Small)
            .when_some(provider_logo(choice.provider), |button, logo| button.icon(logo));
        if !editable {
            return div()
                .id("fixed-commander-settings")
                .child(button.disabled(true))
                .tooltip(|window, cx| {
                    atelier::Tooltip::new(tr(
                        "Model and effort are fixed for this conversation",
                        "この会話のモデルと思考量は固定されています",
                    ))
                    .build(window, cx)
                })
                .into_any_element();
        }
        let view = cx.entity();
        AnchoredPopover::new("composer-settings-popover")
            .anchor(Corner::BottomLeft)
            .appearance(false)
            .track_focus(&self.commander_settings_focus)
            .open(self.commander_settings_open)
            .on_open_change(cx.listener(|this, open, window, cx| {
                this.commander_settings_open = *open;
                if !open {
                    this.commander_models_open = false;
                    this.fields.composer.read(cx).focus_handle(cx).focus(window);
                }
                cx.notify();
            }))
            .trigger(Trigger {
                selected: self.commander_settings_open,
                button: button
                    .tooltip(tr("Choose model and reasoning effort", "モデルと思考量を選択"))
                    .icon_after(if self.commander_settings_open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronUp
                    })
                    .on_click(cx.listener(|this, event, window, cx| {
                        // Pointer activation is owned by Popover; keyboard activation is ours.
                        if matches!(event, ClickEvent::Keyboard(_)) {
                            this.commander_settings_open = !this.commander_settings_open;
                            this.commander_models_open = false;
                            if this.commander_settings_open {
                                this.commander_settings_focus.focus(window);
                            }
                            cx.notify();
                        }
                    })),
            })
            .content(move |_, _, cx| {
                view.update(cx, |this, cx| {
                    div()
                        .id("composer-settings-focus")
                        .track_focus(&this.commander_settings_focus)
                        .child(this.composer_settings_panel(cx))
                })
            })
            .into_any_element()
    }

    fn set_commander_effort(&mut self, effort: Effort, cx: &mut Context<Self>) {
        self.commander_model.effort = effort;
        cx.notify();
    }

    fn composer_settings_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.commander_models_open {
            return self.composer_model_list(cx);
        }
        let theme = cx.theme().clone();
        let choice = self.commander_choice();
        let selected = EFFORTS.iter().position(|effort| *effort == choice.effort).unwrap_or(1);
        let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
        let measured = bounds.clone();
        let clicked = bounds.clone();
        let steps = EFFORTS.into_iter().enumerate().map(|(index, effort)| {
            div().flex().flex_1().justify_center().child(
                Button::icon_only(
                    ("commander-effort", index),
                    if index == selected { IconName::Check } else { IconName::Record },
                )
                .ghost()
                .selected(index == selected)
                .tint(theme.colors.text)
                .radius(theme.density.control_md / 2.)
                .tooltip(trf!("Reasoning effort: {}", "思考量: {}", effort_label(effort)))
                .on_click(cx.listener(move |this, _, _, cx| this.set_commander_effort(effort, cx))),
            )
        });
        Popover::new()
            .w(theme.typography.size_md * 25.)
            .p(theme.spacing.lg)
            .gap(theme.spacing.md)
            .rounded(theme.shape.radius_lg)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme.spacing.sm)
                    .child(Icon::new(IconName::Sparkles).color(theme.colors.accent_text))
                    .child(
                        div().flex().flex_1().justify_center().child(
                            Label::new(effort_label(choice.effort))
                                .size(TextSize::XLarge)
                                .color(TextColor::Accent),
                        ),
                    )
                    .child(
                        Button::icon_only("reset-commander-effort", IconName::Refresh)
                            .ghost()
                            .size(ControlSize::Small)
                            .disabled(choice.effort == Effort::Medium)
                            .tooltip(tr("Reset reasoning effort", "思考量を標準に戻す"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_commander_effort(Effort::Medium, cx)
                            })),
                    ),
            )
            .child(
                div().flex().justify_center().child(
                    Button::new("choose-commander-model", model_label(&choice).to_owned())
                        .ghost()
                        .size(ControlSize::Small)
                        .icon_after(IconName::ChevronRight)
                        .when_some(provider_logo(choice.provider), |button, logo| button.icon(logo))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.commander_models_open = true;
                            this.commander_settings_focus.focus(window);
                            cx.notify();
                        })),
                ),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .p(theme.spacing.xxs)
                    .rounded(theme.density.control_md)
                    .bg(theme.colors.elevated)
                    .border_1()
                    .border_color(theme.colors.border)
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .h_full()
                            .w(relative((selected as f32 + 0.5) / EFFORTS.len() as f32))
                            .rounded(theme.density.control_md)
                            .bg(theme.colors.accent),
                    )
                    .children(steps)
                    .child(
                        canvas(move |bounds, _, _| measured.set(Some(bounds)), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            if let Some(bounds) = clicked.get() {
                                this.set_commander_effort(effort_at(event.position.x, bounds), cx);
                            }
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                        if event.pressed_button == Some(MouseButton::Left)
                            && let Some(bounds) = bounds.get()
                        {
                            this.set_commander_effort(effort_at(event.position.x, bounds), cx);
                        }
                    }))
                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                        let next = match event.keystroke.key.as_str() {
                            "left" => selected.saturating_sub(1),
                            "right" => (selected + 1).min(EFFORTS.len() - 1),
                            "home" => 0,
                            "end" => EFFORTS.len() - 1,
                            _ => return,
                        };
                        this.set_commander_effort(EFFORTS[next], cx);
                        cx.stop_propagation();
                    })),
            )
            .child(div().flex().children(EFFORTS.map(|effort| {
                div().flex().flex_1().justify_center().child(
                    Label::new(effort_label(effort)).size(TextSize::XSmall).color(TextColor::Muted),
                )
            })))
            .into_any_element()
    }

    fn composer_model_list(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let current = self.commander_choice();
        let mut rows = Vec::new();
        for provider in self.allowed_providers() {
            rows.push(
                div()
                    .px(theme.spacing.sm)
                    .pt(theme.spacing.sm)
                    .child(Label::caption(provider.label()))
                    .into_any_element(),
            );
            let default_grade = self
                .mirror
                .router
                .catalog
                .models
                .iter()
                .find(|profile| profile.provider == provider && profile.model.is_empty())
                .map_or(ModelGrade::Standard, |profile| profile.grade);
            let mut choices = vec![(String::new(), default_grade)];
            for profile in &self.mirror.router.catalog.models {
                if profile.provider == provider
                    && !choices.iter().any(|(name, _)| name == &profile.model)
                {
                    choices.push((profile.model.clone(), profile.grade));
                }
            }
            for (index, (model, grade)) in choices.into_iter().enumerate() {
                let selected = current.provider == provider && current.model == model;
                let label = if model.is_empty() {
                    tr("Default model", "既定のモデル").to_owned()
                } else {
                    model.clone()
                };
                rows.push(
                    ListItem::new(
                        gpui::ElementId::Name(
                            format!("commander-model-{provider:?}-{index}").into(),
                        ),
                        label,
                    )
                    .selected(selected)
                    .when_some(provider_logo(provider), |item, logo| item.icon(logo))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.allowed_providers().contains(&provider) {
                            this.commander_model = ModelChoice {
                                provider,
                                model: model.clone(),
                                grade,
                                effort: this.commander_model.effort,
                            };
                            this.commander_models_open = false;
                            this.commander_settings_focus.focus(window);
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
                );
            }
        }
        Popover::new()
            .w(theme.typography.size_md * 25.)
            .p(theme.spacing.sm)
            .rounded(theme.shape.radius_lg)
            .child(
                Button::new("back-to-commander-effort", tr("Model", "モデル"))
                    .ghost()
                    .icon(IconName::ChevronLeft)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.commander_models_open = false;
                        this.commander_settings_focus.focus(window);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("commander-model-list")
                    .flex()
                    .flex_col()
                    .max_h(theme.density.row * 10.)
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }
}

fn effort_at(x: Pixels, bounds: Bounds<Pixels>) -> Effort {
    let position = ((x - bounds.left()) / bounds.size.width.max(gpui::px(1.))).clamp(0., 1.);
    EFFORTS[((position * EFFORTS.len() as f32) as usize).min(EFFORTS.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_past_the_track_clamps_to_the_end_levels() {
        let bounds = Bounds::new(
            gpui::point(gpui::px(100.), gpui::px(0.)),
            gpui::size(gpui::px(400.), gpui::px(32.)),
        );
        assert_eq!(
            (effort_at(gpui::px(0.), bounds), effort_at(gpui::px(600.), bounds)),
            (Effort::Low, Effort::Max)
        );
    }

    #[test]
    fn allowed_model_keeps_its_name_grade_and_effort() {
        let choice = ModelChoice {
            provider: ProviderKind::Codex,
            model: "configured-model".into(),
            grade: ModelGrade::Frontier,
            effort: Effort::High,
        };
        assert_eq!(allowed_choice(&choice, &[ProviderKind::Codex]), choice);
    }

    #[test]
    fn changing_allowed_provider_resets_provider_specific_model() {
        let choice = ModelChoice {
            provider: ProviderKind::Codex,
            model: "configured-model".into(),
            grade: ModelGrade::Frontier,
            effort: Effort::High,
        };
        assert_eq!(
            allowed_choice(&choice, &[ProviderKind::ClaudeCode]),
            ModelChoice {
                provider: ProviderKind::ClaudeCode,
                model: String::new(),
                grade: ModelGrade::Standard,
                effort: Effort::High
            }
        );
    }
}
