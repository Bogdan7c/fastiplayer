//! Зарезервированный Undo slot: visibility, interaction, accessibility и intent.

use std::time::Duration;

use animation_core::Easing;
use animation_core::visibility::{VisibilityEffect, VisibilitySample};
use egui::{Color32, Key, Rect, Response, Sense, Ui, WidgetInfo, WidgetType};
use ui_artwork_egui::{
    ArtworkPainter, PlaylistToolbarButtonStyle, PlaylistToolbarGlyph, PlaylistToolbarPaintState,
};

use crate::playlist_runtime::{PlaylistUndoUiSnapshot, RemovalUndoUiModel};
use crate::ui::animation::{
    VisibilityAnimation, VisibilityAnimationSpec, VisibilityMotion, VisibilityTarget,
};
use crate::ui::skin::PlaylistToolbarStyle;

use super::super::super::PlaylistUiOutput;
use super::super::super::actions::PlaylistAction;

/// Stable egui Id visibility-анимации зарезервированного Undo slot.
const VISIBILITY_ID_SUFFIX: &str = "playlist_toolbar_undo_visibility";
/// Stable widget Id интерактивной Undo-кнопки не зависит от countdown.
const WIDGET_ID_SUFFIX: &str = "playlist_toolbar_undo";
/// Полная длительность появления и точного обратного исчезновения.
const VISIBILITY_DURATION: Duration = Duration::from_millis(180);
/// Полностью скрытый glyph сохраняет 80% content scale.
const HIDDEN_CONTENT_SCALE: f32 = 0.80;

/// Рисует Undo внутри rect, который основной toolbar резервирует всегда.
pub(super) fn show(
    ui: &mut Ui,
    rect: Rect,
    snapshot: &PlaylistUndoUiSnapshot,
    style: PlaylistToolbarStyle,
    motion: VisibilityMotion,
    output: &mut PlaylistUiOutput,
) {
    // Animator вызывается на каждом активном кадре toolbar, даже без Undo:
    // первый authoritative кадр поэтому начинает настоящий fade-in из нуля.
    let target = if snapshot.undo.is_some() {
        VisibilityTarget::Visible
    } else {
        VisibilityTarget::Hidden
    };
    // Named spec фиксирует единый 180-ms reversible UX-контракт.
    let visibility = VisibilityAnimation::new(
        ui,
        VISIBILITY_ID_SUFFIX,
        VisibilityAnimationSpec {
            target,
            duration: VISIBILITY_DURATION,
            easing: Easing::EaseOutCubic,
            effect: VisibilityEffect::FadeScale {
                hidden_scale: HIDDEN_CONTENT_SCALE,
            },
            motion,
        },
    )
    .sample(ui);

    // Authoritative Undo получает interaction сразу, независимо от малой opacity.
    if let Some(undo) = snapshot.undo
        && ui.is_enabled()
    {
        let response = render_control(ui, rect, undo, visibility, style);
        if response.clicked() {
            output.push_action(PlaylistAction::UndoRemoval);
        }
    } else if visibility.opacity > 0.0 {
        // Exit tail остаётся только paint: Response, tooltip и AccessKit исчезают сразу.
        paint_residual_glyph(ui, rect, visibility, style);
    }
}

/// Создаёт интерактивную кнопку только при authoritative runtime state.
fn render_control(
    ui: &mut Ui,
    rect: Rect,
    undo: RemovalUndoUiModel,
    visibility: VisibilitySample,
    style: PlaylistToolbarStyle,
) -> Response {
    // Tooltip и AccessKit используют одну строку, включая актуальный countdown.
    let action_label = undo.action_label();
    // Countdown не входит в Id, поэтому keyboard focus стабилен между секундами.
    let widget_id = ui.make_persistent_id(WIDGET_ID_SUFFIX);
    // Полная зарезервированная hit-area доступна уже на первом fade-in кадре.
    let response = ui.interact(rect, widget_id, Sense::click());
    // Custom Painter artwork требует явного Button node и локализованного имени.
    response.widget_info(|| {
        WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), action_label.clone())
    });
    // Pointer click не оставляет декоративный focus outline; keyboard focus сохраняется.
    if response.clicked() && response.interact_pointer_pos().is_some() {
        response.surrender_focus();
    }
    // Interaction state и visibility sample сходятся только в paint boundary.
    paint_interactive_control(ui, rect, style, &response, visibility);
    // Tooltip появляется только у authoritative Response.
    response.on_hover_text(action_label)
}

/// Разрешает app-owned hover/press/focus в toolkit-neutral artwork state.
fn paint_interactive_control(
    ui: &Ui,
    rect: Rect,
    style: PlaylistToolbarStyle,
    response: &Response,
    visibility: VisibilitySample,
) {
    // Keyboard press использует ту же визуальную подложку, что pointer press.
    let keyboard_pressed = response.has_focus()
        && ui.input(|input| input.key_down(Key::Space) || input.key_down(Key::Enter));
    // Нажатие остаётся только transient UI presentation.
    let pressed = response.is_pointer_button_down_on() || keyboard_pressed;
    // Authoritative control находится в enabled UI; проверка сохраняет защиту helper-а.
    let enabled = ui.is_enabled();
    // Foreground полностью определяется skin и текущим interaction state.
    let foreground = if !enabled {
        style.foreground_disabled
    } else if response.hovered() || pressed {
        style.foreground_hover
    } else {
        style.foreground_idle
    };
    // Surface не появляется у disabled или idle glyph.
    let surface_fill = if !enabled {
        Color32::TRANSPARENT
    } else if pressed {
        style.surface_pressed
    } else if response.hovered() {
        style.surface_hover
    } else {
        Color32::TRANSPARENT
    };

    paint(
        ui,
        rect,
        PlaylistToolbarPaintState {
            foreground,
            surface_fill,
            focus_visible: enabled && response.has_focus(),
            opacity: visibility.opacity,
            content_scale: visibility.scale,
        },
        style,
    );
}

/// Рисует остаточный exit glyph без регистрации widget/accessibility semantics.
fn paint_residual_glyph(
    ui: &Ui,
    rect: Rect,
    visibility: VisibilitySample,
    style: PlaylistToolbarStyle,
) {
    paint(
        ui,
        rect,
        PlaylistToolbarPaintState {
            foreground: style.foreground_idle,
            surface_fill: Color32::TRANSPARENT,
            focus_visible: false,
            opacity: visibility.opacity,
            content_scale: visibility.scale,
        },
        style,
    );
}

/// Единая artwork boundary не знает ни про Undo runtime, ни про interaction.
fn paint(ui: &Ui, rect: Rect, paint_state: PlaylistToolbarPaintState, style: PlaylistToolbarStyle) {
    ArtworkPainter::new(ui.painter()).playlist_toolbar_button(
        rect,
        PlaylistToolbarGlyph::Undo,
        paint_state,
        PlaylistToolbarButtonStyle {
            icon_extent: style.icon_extent,
            glyph_stroke_width: style.glyph_stroke_width,
            surface_corner_radius: style.surface_corner_radius,
            focus_outline: style.focus_outline,
            focus_inset: style.focus_inset,
        },
    );
}

#[cfg(test)]
mod tests {
    use egui::{Context, Event, Key, Modifiers, PointerButton, RawInput, Rect, pos2, vec2};

    use super::*;
    use crate::ui::skin::{MinimalSkin, PlayerSkin};

    /// Стабильная 32x32-point hit-area совпадает с production toolbar slot.
    fn undo_rect() -> Rect {
        Rect::from_min_size(pos2(20.0, 20.0), vec2(32.0, 32.0))
    }

    /// Создаёт authoritative Undo presentation для toolbar-тестов.
    fn undo_snapshot(seconds_remaining: u64) -> PlaylistUndoUiSnapshot {
        PlaylistUndoUiSnapshot {
            undo: Some(RemovalUndoUiModel {
                kind_label: "очистку плейлиста",
                seconds_remaining,
            }),
            next_wake_deadline: None,
        }
    }

    /// Создаёт deterministic viewport для настоящего egui interaction pass.
    fn raw_input(events: Vec<Event>, time: f64) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(120.0, 80.0))),
            time: Some(time),
            predicted_dt: 0.01,
            events,
            ..RawInput::default()
        }
    }

    /// Строит один pointer press/release event.
    fn pointer_button(position: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    /// Создаёт keyboard press/release в одном deterministic frame.
    fn keyboard_input(key: Key) -> RawInput {
        raw_input(
            vec![
                Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                },
                Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                },
            ],
            0.0,
        )
    }

    /// Рендерит production Undo owner и возвращает typed actions кадра.
    fn render_input(
        context: &Context,
        snapshot: &PlaylistUndoUiSnapshot,
        motion: VisibilityMotion,
        input: RawInput,
    ) -> Vec<PlaylistAction> {
        let mut output = PlaylistUiOutput::default();
        let _ = context.run_ui(input, |ui| {
            show(
                ui,
                undo_rect(),
                snapshot,
                MinimalSkin.playlist_toolbar_style(),
                motion,
                &mut output,
            );
        });
        output.take_actions()
    }

    /// Изолирует production control для keyboard focus/activation regression.
    fn control_frame(context: &Context, input: RawInput) -> (Vec<PlaylistAction>, bool) {
        let mut output = PlaylistUiOutput::default();
        let mut has_focus = false;
        let _ = context.run_ui(input, |ui| {
            let response = render_control(
                ui,
                undo_rect(),
                undo_snapshot(4).undo.expect("authoritative Undo fixture"),
                VisibilitySample {
                    opacity: 1.0,
                    scale: 1.0,
                },
                MinimalSkin.playlist_toolbar_style(),
            );
            if response.clicked() {
                output.push_action(PlaylistAction::UndoRemoval);
            }
            has_focus = response.has_focus();
        });
        (output.take_actions(), has_focus)
    }

    #[test]
    fn tooltip_and_accessibility_share_exact_russian_countdown_label() {
        assert_eq!(
            undo_snapshot(4)
                .undo
                .expect("authoritative Undo fixture")
                .action_label(),
            "Отменить очистку плейлиста (4 с)"
        );
    }

    #[test]
    fn pointer_click_publishes_exact_playlist_action_during_fade_in() {
        let context = Context::default();
        let position = undo_rect().center();
        let hidden_snapshot = PlaylistUndoUiSnapshot {
            undo: None,
            next_wake_deadline: None,
        };
        let active_snapshot = undo_snapshot(4);

        // Первый hidden кадр регистрирует нулевую animation position.
        assert!(
            render_input(
                &context,
                &hidden_snapshot,
                VisibilityMotion::Standard,
                raw_input(Vec::new(), 0.0),
            )
            .is_empty()
        );
        // Даже ранний fade-in кадр уже создаёт полную authoritative hit-area.
        assert!(
            render_input(
                &context,
                &active_snapshot,
                VisibilityMotion::Standard,
                raw_input(vec![Event::PointerMoved(position)], 0.01),
            )
            .is_empty()
        );
        assert!(
            render_input(
                &context,
                &active_snapshot,
                VisibilityMotion::Standard,
                raw_input(vec![pointer_button(position, true)], 0.02),
            )
            .is_empty()
        );
        assert_eq!(
            render_input(
                &context,
                &active_snapshot,
                VisibilityMotion::Standard,
                raw_input(vec![pointer_button(position, false)], 0.03),
            ),
            vec![PlaylistAction::UndoRemoval]
        );
    }

    #[test]
    fn keyboard_focus_allows_space_and_enter_activation() {
        for activation_key in [Key::Space, Key::Enter] {
            let context = Context::default();
            let _ = control_frame(&context, RawInput::default());
            let (_, tab_focused) = control_frame(&context, keyboard_input(Key::Tab));
            let (actions, still_focused) = control_frame(&context, keyboard_input(activation_key));

            assert!(tab_focused);
            assert!(still_focused);
            assert_eq!(actions, vec![PlaylistAction::UndoRemoval]);
        }
    }

    #[test]
    fn exit_tail_has_no_interaction_after_authoritative_state_disappears() {
        let context = Context::default();
        let position = undo_rect().center();
        let active_snapshot = undo_snapshot(4);
        let hidden_snapshot = PlaylistUndoUiSnapshot {
            undo: None,
            next_wake_deadline: None,
        };

        // Первый visible вызов инициализирует полное состояние, чтобы exit был видимым.
        assert!(
            render_input(
                &context,
                &active_snapshot,
                VisibilityMotion::Standard,
                raw_input(Vec::new(), 0.0),
            )
            .is_empty()
        );
        // Во время последующего fade-out на месте slot нет Response.
        for (time, events) in [
            (0.01, vec![Event::PointerMoved(position)]),
            (0.02, vec![pointer_button(position, true)]),
            (0.03, vec![pointer_button(position, false)]),
        ] {
            assert!(
                render_input(
                    &context,
                    &hidden_snapshot,
                    VisibilityMotion::Standard,
                    raw_input(events, time),
                )
                .is_empty()
            );
        }
    }

    #[test]
    fn reduced_motion_hides_residual_glyph_instantly() {
        let context = Context::default();
        let active_snapshot = undo_snapshot(4);
        let hidden_snapshot = PlaylistUndoUiSnapshot {
            undo: None,
            next_wake_deadline: None,
        };

        let active_output = context.run_ui(raw_input(Vec::new(), 0.0), |ui| {
            let mut output = PlaylistUiOutput::default();
            show(
                ui,
                undo_rect(),
                &active_snapshot,
                MinimalSkin.playlist_toolbar_style(),
                VisibilityMotion::Reduced,
                &mut output,
            );
        });
        let hidden_output = context.run_ui(raw_input(Vec::new(), 0.01), |ui| {
            let mut output = PlaylistUiOutput::default();
            show(
                ui,
                undo_rect(),
                &hidden_snapshot,
                MinimalSkin.playlist_toolbar_style(),
                VisibilityMotion::Reduced,
                &mut output,
            );
        });

        assert!(!active_output.shapes.is_empty());
        assert!(hidden_output.shapes.is_empty());
    }
}
