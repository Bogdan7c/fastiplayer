//! Focused tests persistent Shuffle/Repeat controls.

use super::*;
use crate::playlist_runtime::{NavigationControlAvailability, PlaylistTransportUiModel};
use crate::ui::skin::{ControlsStyle, MinimalSkin, PlayerSkin};
use egui::{Event, Key, Modifiers, PointerButton, RawInput, Rect, Vec2, pos2, vec2};
use playlist_core::RepeatMode;
use ui_artwork_egui::QueueModeGlyph;

/// Создаёт queue-mode snapshot без навигационных целей, как у пустой очереди.
fn transport_model() -> PlaylistTransportUiModel {
    PlaylistTransportUiModel {
        playlist_view_revision: 1,
        previous: NavigationControlAvailability::Disabled,
        next: NavigationControlAvailability::Disabled,
        repeat_mode: RepeatMode::StopAtEnd,
        shuffle_enabled: false,
        queue_modes_enabled: true,
        global_status: None,
    }
}

#[test]
fn repeat_cycle_and_labels_cover_all_authoritative_states() {
    let mut model = transport_model();

    assert_eq!(next_repeat_mode(model.repeat_mode), RepeatMode::RepeatQueue);
    assert_eq!(
        QueueModeControl::Repeat.action(&model),
        TransportControlAction::SetRepeatMode {
            mode: RepeatMode::RepeatQueue
        }
    );
    assert_eq!(
        QueueModeControl::Repeat.accessible_label(&model),
        "Повтор выключен. Включить повтор очереди"
    );

    model.repeat_mode = RepeatMode::RepeatQueue;
    assert_eq!(next_repeat_mode(model.repeat_mode), RepeatMode::RepeatOne);
    assert_eq!(
        QueueModeControl::Repeat.accessible_label(&model),
        "Повтор очереди включён. Включить повтор одного трека"
    );

    model.repeat_mode = RepeatMode::RepeatOne;
    assert_eq!(next_repeat_mode(model.repeat_mode), RepeatMode::StopAtEnd);
    assert_eq!(
        QueueModeControl::Repeat.glyph(&model),
        QueueModeGlyph::RepeatOne
    );
    assert_eq!(
        QueueModeControl::Repeat.accessible_label(&model),
        "Повтор одного трека включён. Выключить повтор"
    );
}

#[test]
fn shuffle_action_is_exact_inverse_of_authoritative_snapshot() {
    let mut model = transport_model();

    assert_eq!(
        QueueModeControl::Shuffle.action(&model),
        TransportControlAction::SetShuffleEnabled { enabled: true }
    );
    model.shuffle_enabled = true;
    assert_eq!(
        QueueModeControl::Shuffle.action(&model),
        TransportControlAction::SetShuffleEnabled { enabled: false }
    );
}

#[test]
fn persistent_style_tokens_resolve_exact_endpoint_colors_and_scale() {
    let style = MinimalSkin.controls_style().persistent_control;
    let active_hover_pressed = resolve_paint_state(style, true, true, false, 1.0, 1.0, 1.0);
    let disabled_active = resolve_paint_state(style, false, false, false, 0.0, 1.0, 0.0);
    let reduced_pressed = resolve_paint_state(style, true, false, true, 0.0, 0.0, 1.0);

    assert_eq!(active_hover_pressed.foreground, style.foreground_active);
    assert_eq!(active_hover_pressed.surface_fill, style.surface_pressed);
    assert_eq!(active_hover_pressed.content_scale, PRESSED_CONTENT_SCALE);
    assert!(active_hover_pressed.focus_visible);
    assert_eq!(disabled_active.foreground, style.foreground_disabled);
    assert_eq!(disabled_active.surface_fill, style.surface_active);
    assert_eq!(reduced_pressed.content_scale, 1.0);
}

/// Собирает production layout для content row окна с обычными panel margins.
fn layout_for_window_width(
    window_width: f32,
) -> (
    Rect,
    Rect,
    Rect,
    Rect,
    QueueModeControlLayout,
    ControlsStyle,
) {
    let controls_style = MinimalSkin.controls_style();
    let row_rect = Rect::from_min_size(
        pos2(10.0, 0.0),
        vec2(window_width - 20.0, controls_style.playback_button_diameter),
    );
    let playback_rect = super::super::playback_button_anchor_rect(row_rect, controls_style);
    let open_rect = super::super::open_file_button_anchor_rect(row_rect, controls_style);
    let fullscreen_rect = super::super::fullscreen_button_anchor_rect(row_rect, controls_style);
    let queue_layout = control_layout(
        playback_rect,
        open_rect,
        fullscreen_rect,
        controls_style,
        8.0,
    );

    (
        row_rect,
        playback_rect,
        open_rect,
        fullscreen_rect,
        queue_layout,
        controls_style,
    )
}

#[test]
fn queue_mode_layout_is_symmetric_stable_and_non_overlapping_at_required_widths() {
    for window_width in [400.0, 442.0, 640.0] {
        let (row_rect, playback_rect, open_rect, fullscreen_rect, queue_layout, controls_style) =
            layout_for_window_width(window_width);
        let base_next_rect =
            super::super::transport::next_button_rect(playback_rect, controls_style);
        let item_spacing = 8.0;
        let volume_zone = super::super::volume_controls_zone_rect(
            row_rect,
            open_rect,
            queue_layout.shuffle_rect,
            item_spacing,
        );

        assert_eq!(playback_rect.center().x, row_rect.center().x);
        assert!(
            (playback_rect.center().x
                - queue_layout.shuffle_rect.center().x
                - (queue_layout.repeat_rect.center().x - playback_rect.center().x))
                .abs()
                < 0.0001
        );
        assert_eq!(
            queue_layout.shuffle_rect.size(),
            Vec2::splat(controls_style.transport_button_size)
        );
        assert_eq!(
            queue_layout.repeat_rect.size(),
            Vec2::splat(controls_style.transport_button_size)
        );
        assert!(
            open_rect.right() + controls_style.queue_mode_neighbor_gap
                <= queue_layout.shuffle_rect.left()
        );
        assert!(
            queue_layout.repeat_rect.right() + controls_style.queue_mode_neighbor_gap
                <= fullscreen_rect.left()
        );
        assert!(volume_zone.right() + item_spacing <= queue_layout.shuffle_rect.left());

        for reveal_progress in [0.0, 0.5, 1.0] {
            let rate_layout = super::super::playback_rate::control_layout(
                playback_rect,
                base_next_rect,
                queue_layout.repeat_rect,
                controls_style,
                item_spacing,
                reveal_progress,
            );

            assert!(
                rate_layout.next_button_rect.right() + controls_style.queue_mode_neighbor_gap
                    <= queue_layout.repeat_rect.left()
            );
            assert_eq!(
                queue_layout,
                control_layout(
                    playback_rect,
                    open_rect,
                    fullscreen_rect,
                    controls_style,
                    item_spacing
                )
            );
        }
    }
}

#[test]
fn narrow_widths_reduce_only_external_distance_and_rate_capacity() {
    let (_, playback_400, _, _, queue_400, controls_style) = layout_for_window_width(400.0);
    let (_, playback_442, _, _, queue_442, _) = layout_for_window_width(442.0);
    let (_, playback_640, _, _, queue_640, _) = layout_for_window_width(640.0);

    let distance_400 = queue_400.repeat_rect.center().x - playback_400.center().x;
    let distance_442 = queue_442.repeat_rect.center().x - playback_442.center().x;
    let distance_640 = queue_640.repeat_rect.center().x - playback_640.center().x;

    assert!(distance_400 < distance_442);
    assert!(distance_442 < distance_640);
    assert_eq!(
        distance_640,
        controls_style.queue_mode_button_center_distance
    );

    let full_rate_layout = super::super::playback_rate::control_layout(
        playback_640,
        super::super::transport::next_button_rect(playback_640, controls_style),
        queue_640.repeat_rect,
        controls_style,
        8.0,
        1.0,
    );
    let narrow_rate_layout = super::super::playback_rate::control_layout(
        playback_400,
        super::super::transport::next_button_rect(playback_400, controls_style),
        queue_400.repeat_rect,
        controls_style,
        8.0,
        1.0,
    );

    assert_eq!(
        full_rate_layout.button_rect.width(),
        controls_style.playback_rate_button_width
    );
    assert!(narrow_rate_layout.button_rect.width() < full_rate_layout.button_rect.width());
}

#[test]
fn exact_intents_do_not_mutate_authoritative_ui_snapshot_optimistically() {
    let model = transport_model();

    assert_eq!(
        QueueModeControl::Shuffle.action(&model),
        TransportControlAction::SetShuffleEnabled { enabled: true }
    );
    assert_eq!(
        QueueModeControl::Repeat.action(&model),
        TransportControlAction::SetRepeatMode {
            mode: RepeatMode::RepeatQueue
        }
    );
    assert!(!model.shuffle_enabled);
    assert_eq!(model.repeat_mode, RepeatMode::StopAtEnd);
}

/// Запускает один synthetic animation frame через production helper.
fn animation_frame(
    context: &egui::Context,
    time_seconds: f64,
    frame_delta_seconds: f32,
    target: bool,
    duration_seconds: f32,
) -> f32 {
    let input = RawInput {
        time: Some(time_seconds),
        predicted_dt: frame_delta_seconds,
        ..RawInput::default()
    };
    let mut progress = f32::NAN;
    let _ = context.run_ui(input, |ui| {
        progress = animated_bool(
            ui,
            ui.make_persistent_id("queue-mode-animation-test"),
            target,
            duration_seconds,
        );
    });
    assert!(progress.is_finite());
    progress
}

#[test]
fn hover_active_and_pressed_use_exact_transition_durations() {
    for duration_seconds in [
        HOVER_TRANSITION_SECONDS,
        ACTIVE_TRANSITION_SECONDS,
        PRESSED_TRANSITION_SECONDS,
    ] {
        let context = egui::Context::default();
        let hidden = animation_frame(
            &context,
            0.0,
            duration_seconds * 0.5,
            false,
            duration_seconds,
        );
        let halfway = animation_frame(
            &context,
            f64::from(duration_seconds * 0.5),
            duration_seconds * 0.5,
            true,
            duration_seconds,
        );
        let complete = animation_frame(
            &context,
            f64::from(duration_seconds),
            duration_seconds * 0.5,
            true,
            duration_seconds,
        );

        assert_eq!(hidden, 0.0);
        assert!((halfway - 0.5).abs() < 0.0001);
        assert_eq!(complete, 1.0);
    }
}

#[test]
fn active_transition_reverses_from_current_progress_without_jump() {
    let context = egui::Context::default();
    let _ = animation_frame(
        &context,
        0.0,
        ACTIVE_TRANSITION_SECONDS * 0.5,
        false,
        ACTIVE_TRANSITION_SECONDS,
    );
    let halfway = animation_frame(
        &context,
        f64::from(ACTIVE_TRANSITION_SECONDS * 0.5),
        ACTIVE_TRANSITION_SECONDS * 0.5,
        true,
        ACTIVE_TRANSITION_SECONDS,
    );
    let reversed = animation_frame(
        &context,
        f64::from(ACTIVE_TRANSITION_SECONDS * 0.75),
        ACTIVE_TRANSITION_SECONDS * 0.25,
        false,
        ACTIVE_TRANSITION_SECONDS,
    );

    assert!((halfway - 0.5).abs() < 0.0001);
    assert!(reversed > 0.0);
    assert!(reversed < halfway);
}

#[test]
fn settled_transition_does_not_keep_requesting_repaint() {
    let context = egui::Context::default();
    let _ = animation_frame(
        &context,
        0.0,
        ACTIVE_TRANSITION_SECONDS,
        false,
        ACTIVE_TRANSITION_SECONDS,
    );
    let _ = animation_frame(
        &context,
        f64::from(ACTIVE_TRANSITION_SECONDS),
        ACTIVE_TRANSITION_SECONDS,
        true,
        ACTIVE_TRANSITION_SECONDS,
    );
    let settled = animation_frame(
        &context,
        f64::from(ACTIVE_TRANSITION_SECONDS * 2.0),
        ACTIVE_TRANSITION_SECONDS,
        true,
        ACTIVE_TRANSITION_SECONDS,
    );

    assert_eq!(settled, 1.0);
    assert!(!context.requested_repaint_last_pass());
}

/// Рендерит один control frame и возвращает actions/focus настоящего egui widget.
fn control_frame(
    context: &egui::Context,
    input: RawInput,
    model: &PlaylistTransportUiModel,
    control: QueueModeControl,
) -> (Vec<ControlAction>, bool) {
    let mut actions = Vec::new();
    let mut has_focus = false;
    let _ = context.run_ui(input, |ui| {
        let response = render_control(
            ui,
            Rect::from_min_size(pos2(20.0, 20.0), vec2(32.0, 32.0)),
            control,
            model,
            MinimalSkin.controls_style(),
            true,
            &mut actions,
        );
        has_focus = response.has_focus();
    });
    (actions, has_focus)
}

/// Создаёт один keyboard press/release frame.
fn keyboard_input(key: Key) -> RawInput {
    RawInput {
        events: vec![
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            },
            Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::default(),
            },
        ],
        ..RawInput::default()
    }
}

/// Создаёт pointer hover внутри production 32×32 hit-area.
fn pointer_hover_input() -> RawInput {
    RawInput {
        events: vec![Event::PointerMoved(pos2(36.0, 36.0))],
        ..RawInput::default()
    }
}

/// Создаёт отдельный pointer press или release frame.
fn pointer_button_input(pressed: bool) -> RawInput {
    RawInput {
        events: vec![Event::PointerButton {
            pos: pos2(36.0, 36.0),
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        }],
        ..RawInput::default()
    }
}

/// Выполняет полный egui hover → press → release цикл одного queue-mode control.
fn click_control(
    context: &egui::Context,
    model: &PlaylistTransportUiModel,
    control: QueueModeControl,
) -> (Vec<ControlAction>, bool) {
    let (hover_actions, _) = control_frame(context, pointer_hover_input(), model, control);
    let (press_actions, _) = control_frame(context, pointer_button_input(true), model, control);
    assert!(hover_actions.is_empty());
    assert!(press_actions.is_empty());
    control_frame(context, pointer_button_input(false), model, control)
}

#[test]
fn disabled_active_and_inactive_controls_never_emit_intents() {
    for (shuffle_enabled, repeat_mode) in [
        (false, RepeatMode::StopAtEnd),
        (true, RepeatMode::RepeatOne),
    ] {
        let mut model = transport_model();
        model.queue_modes_enabled = false;
        model.shuffle_enabled = shuffle_enabled;
        model.repeat_mode = repeat_mode;
        let context = egui::Context::default();

        let (shuffle_actions, _) = click_control(&context, &model, QueueModeControl::Shuffle);
        let (repeat_actions, _) = click_control(&context, &model, QueueModeControl::Repeat);

        assert!(shuffle_actions.is_empty());
        assert!(repeat_actions.is_empty());
        assert_eq!(QueueModeControl::Shuffle.selected(&model), shuffle_enabled);
        assert_eq!(
            QueueModeControl::Repeat.selected(&model),
            repeat_mode != RepeatMode::StopAtEnd
        );
    }
}

#[test]
fn pointer_click_emits_intent_without_persistent_focus_outline() {
    let model = transport_model();
    let context = egui::Context::default();

    let (actions, has_focus) = click_control(&context, &model, QueueModeControl::Shuffle);

    assert_eq!(
        actions,
        vec![ControlAction::Transport(
            TransportControlAction::SetShuffleEnabled { enabled: true }
        )]
    );
    assert!(!has_focus);
}

#[test]
fn tab_focus_allows_space_and_enter_activation() {
    for activation_key in [Key::Space, Key::Enter] {
        let model = transport_model();
        let context = egui::Context::default();
        let _ = control_frame(
            &context,
            RawInput::default(),
            &model,
            QueueModeControl::Repeat,
        );
        let (_, tab_focused) = control_frame(
            &context,
            keyboard_input(Key::Tab),
            &model,
            QueueModeControl::Repeat,
        );
        let (actions, still_focused) = control_frame(
            &context,
            keyboard_input(activation_key),
            &model,
            QueueModeControl::Repeat,
        );

        assert!(tab_focused);
        assert!(still_focused);
        assert_eq!(
            actions,
            vec![ControlAction::Transport(
                TransportControlAction::SetRepeatMode {
                    mode: RepeatMode::RepeatQueue
                }
            )]
        );
    }
}
