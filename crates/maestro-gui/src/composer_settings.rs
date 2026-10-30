//! The composer's model and reasoning controls, grouped in one anchored popover.

use atelier::agent::EffortPicker;
use atelier::{ActiveTheme, Button, ControlSize, IconName, IconSource, Label, MenuItem, Popover};
use gpui::{
    App, ClickEvent, Context, Corner, Focusable, IntoElement, ParentElement, RenderOnce, Styled,
    Window, div, prelude::*,
};
use gpui_component::{Selectable, popover::Popover as AnchoredPopover};
use maestro_core::{Effort, ModelChoice, ModelGrade, ProviderKind};

use crate::app::Maestro;
use crate::chat::provider_logo;
use crate::i18n::tr;

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
        EffortPicker::new("commander-effort-picker", EFFORTS.map(effort_label), selected)
            .width(theme.density.form_width / 2.)
            .default_level(1)
            .show_fast(false)
            .model(model_label(&choice).to_owned())
            .when_some(provider_logo(choice.provider), |picker, logo| picker.model_icon(logo))
            .on_select(cx.listener(|this, index: &usize, _, cx| {
                this.set_commander_effort(EFFORTS[*index], cx);
            }))
            .on_model(cx.listener(|this, _, window, cx| {
                this.commander_models_open = true;
                this.commander_settings_focus.focus(window);
                cx.notify();
            }))
            .on_reset(cx.listener(|this, _, _, cx| {
                this.set_commander_effort(Effort::Medium, cx);
            }))
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
                    MenuItem::new(
                        gpui::ElementId::Name(
                            format!("commander-model-{provider:?}-{index}").into(),
                        ),
                        label,
                    )
                    .checked(selected)
                    .icon(
                        provider_logo(provider)
                            .map(IconSource::from)
                            .unwrap_or_else(|| IconName::Code.into()),
                    )
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
            .w(theme.density.form_width / 2.)
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

#[cfg(test)]
mod tests {
    use super::*;

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
