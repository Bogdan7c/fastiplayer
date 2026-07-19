#![cfg(test)]

use std::path::PathBuf;

use egui::{Context, Event, Key, Modifiers, PointerButton, RawInput};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue,
};

use super::*;
use crate::playlist_runtime::{
    PlaylistLoadingView, PlaylistProgressCancelScope, PlaylistProgressModel,
};

/// Строит минимальную read model для status layout.
fn playlist_model(item_count: usize, loading: PlaylistLoadingView) -> PlaylistViewModel {
    let mut queue = PlaylistQueue::new();
    if item_count > 0 {
        let drafts = (0..item_count)
            .map(|index| {
                PlaylistItemDraft::local(
                    LocalLocator::Native(PathBuf::from(format!("status-{index}.mp3"))),
                    None,
                    CachedPlaylistMetadata::new(
                        format!("status-{index}.mp3"),
                        PlaylistMediaKind::Audio,
                    ),
                )
            })
            .collect();
        queue
            .append_batch(drafts)
            .expect("status test queue must fit the playlist hard cap");
    }
    PlaylistViewModel::for_queue_with_revision(&queue, 1, loading)
}

/// Возвращает полную высоту status owner-а в мгновенном reduced-motion режиме.
fn playlist_status_height(
    model: &PlaylistViewModel,
    interaction: &PlaylistInteractionModel,
) -> f32 {
    let mut height = 0.0;
    egui::__run_test_ui(|ui| {
        let mut state = PlaylistUiState::default();
        let mut output = PlaylistUiOutput::default();
        let top_before_status = ui.cursor().top();
        show_status(
            ui,
            model,
            interaction,
            UiMotion::Reduced,
            &mut state,
            &mut output,
        );
        height = ui.cursor().top() - top_before_status;
    });
    height
}

/// Возвращает baseline высоты единственного separator перед списком.
fn separator_height() -> f32 {
    let mut height = 0.0;
    egui::__run_test_ui(|ui| {
        let top_before_separator = ui.cursor().top();
        ui.separator();
        height = ui.cursor().top() - top_before_separator;
    });
    height
}

/// Строит одну безопасную loading presentation для чистой математики transition.
fn loading_presentation() -> PlaylistStatusPresentation {
    PlaylistStatusPresentation::from_models(
        &playlist_model(0, PlaylistLoadingView::Loading),
        &PlaylistInteractionModel::default(),
    )
    .expect("loading fixture must create a status presentation")
}

/// Создаёт deterministic input для настоящих pointer/keyboard action tests.
fn raw_input(events: Vec<Event>, time_seconds: f64) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(420.0, 180.0))),
        time: Some(time_seconds),
        predicted_dt: 0.01,
        events,
        ..RawInput::default()
    }
}

/// Строит один primary pointer press/release event.
fn pointer_button(position: egui::Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos: position,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

/// Создаёт keyboard press/release в одном frame.
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

/// Изолирует production status button и возвращает action/geometry/accessibility state.
fn action_frame(
    context: &Context,
    input: RawInput,
    action: PlaylistStatusAction,
    authoritative: bool,
) -> (Vec<super::super::actions::PlaylistAction>, Rect, bool, bool) {
    let mut state = PlaylistUiState::default();
    let mut output = PlaylistUiOutput::default();
    let mut response_state = None;
    let _ = context.run_ui(input, |ui| {
        let mut access = if authoritative {
            PlaylistStatusRenderAccess::Authoritative {
                state: &mut state,
                output: &mut output,
            }
        } else {
            PlaylistStatusRenderAccess::Actionless
        };
        let response = render_status_action(ui, action, &mut access);
        response_state = Some((response.rect, response.has_focus(), response.enabled()));
    });
    let (rect, has_focus, enabled) =
        response_state.expect("status action must create exactly one response");
    (output.take_actions(), rect, has_focus, enabled)
}

#[test]
fn ready_empty_and_populated_queues_keep_only_the_top_separator() {
    let empty_model = playlist_model(0, PlaylistLoadingView::Ready);
    let populated_model = playlist_model(30, PlaylistLoadingView::Ready);
    let baseline_height = separator_height();

    assert_eq!(
        playlist_status_height(&empty_model, &PlaylistInteractionModel::default()),
        baseline_height
    );
    assert_eq!(
        playlist_status_height(&populated_model, &PlaylistInteractionModel::default()),
        baseline_height
    );
}

#[test]
fn single_and_wrapped_multi_line_status_add_content_and_lower_separator() {
    let model = playlist_model(0, PlaylistLoadingView::Loading);
    let single_line_height = playlist_status_height(&model, &PlaylistInteractionModel::default());
    let wrapped_interaction = PlaylistInteractionModel {
        completion_details: Some(
            "Длинная безопасная строка результата должна переноситься внутри одной общей status-области без расширения sidebar"
                .into(),
        ),
        ..PlaylistInteractionModel::default()
    };
    let wrapped_height = playlist_status_height(&model, &wrapped_interaction);

    assert!(single_line_height > separator_height() * 2.0);
    assert!(wrapped_height > single_line_height);
}

#[test]
fn standard_slide_has_fixed_top_moving_bottom_and_exact_mid_flight_reverse() {
    let presentation = loading_presentation();
    let metrics = PlaylistStatusLayoutMetrics {
        content_height: 32.0,
        separator_height: 8.0,
    };
    let mut state = PlaylistStatusAnimationState::default();

    let start = state.advance(Some(presentation.clone()), UiMotion::Standard, 0.0);
    let midpoint = state.advance(
        Some(presentation.clone()),
        UiMotion::Standard,
        STATUS_TRANSITION_DURATION.as_secs_f32() * 0.5,
    );
    let opening_height = metrics.visible_height(midpoint.progress);
    let reversed = state.advance(
        None,
        UiMotion::Standard,
        STATUS_TRANSITION_DURATION.as_secs_f32() * 0.25,
    );
    let reversed_height = metrics.visible_height(reversed.progress);
    let reopened = state.advance(
        Some(presentation),
        UiMotion::Standard,
        STATUS_TRANSITION_DURATION.as_secs_f32() * 0.25,
    );

    assert_eq!(start.progress, 0.0);
    assert!(opening_height > 0.0 && opening_height < metrics.full_height());
    assert!(reversed_height < opening_height);
    assert_eq!(reopened.progress, midpoint.progress);
    let top = 24.0;
    let opening_bottom = top + opening_height;
    let reversed_bottom = top + reversed_height;
    assert_eq!(top, 24.0);
    assert!(reversed_bottom < opening_bottom);
}

#[test]
fn slide_height_is_monotonic_and_repaint_stops_after_180_ms() {
    let presentation = loading_presentation();
    let metrics = PlaylistStatusLayoutMetrics {
        content_height: 48.0,
        separator_height: 8.0,
    };
    let mut state = PlaylistStatusAnimationState::default();
    let mut heights = Vec::new();

    for _ in 0..3 {
        let frame = state.advance(
            Some(presentation.clone()),
            UiMotion::Standard,
            STATUS_TRANSITION_DURATION.as_secs_f32() / 3.0,
        );
        heights.push(metrics.visible_height(frame.progress));
    }
    let settled = state.advance(Some(presentation), UiMotion::Standard, 0.01);

    assert!(heights.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(heights.last().copied(), Some(metrics.full_height()));
    assert!(!settled.needs_repaint);
}

#[test]
fn reduced_motion_opens_and_closes_immediately_without_residual_snapshot() {
    let presentation = loading_presentation();
    let mut state = PlaylistStatusAnimationState::default();

    let opened = state.advance(Some(presentation), UiMotion::Reduced, 0.0);
    let closed = state.advance(None, UiMotion::Reduced, 0.0);

    assert_eq!(opened.progress, 1.0);
    assert!(opened.authoritative);
    assert!(!opened.needs_repaint);
    assert_eq!(closed.progress, 0.0);
    assert!(closed.presentation.is_none());
    assert!(!closed.needs_repaint);
}

#[test]
fn reveal_flow_reserves_exact_height_without_positive_frame_jump() {
    egui::__run_test_ui(|ui| {
        ui.separator();
        let top_boundary = ui.cursor().top();
        let visible_rect = allocate_reveal_rect(ui, 17.5);
        let row_boundary = ui.cursor().top();

        assert_eq!(visible_rect.top(), top_boundary);
        assert!((visible_rect.height() - 17.5).abs() < 0.1);
        assert!((row_boundary - top_boundary - 17.5).abs() < 0.1);
    });
}

#[test]
fn authoritative_status_actions_support_pointer_space_and_enter() {
    let progress_action =
        PlaylistStatusAction::CancelProgress(PlaylistProgressCancelScope::ManualAdd);
    let pointer_context = Context::default();
    let (_, retry_rect, _, _) = action_frame(
        &pointer_context,
        raw_input(Vec::new(), 0.0),
        PlaylistStatusAction::RetrySave,
        true,
    );
    let pointer_position = retry_rect.center();
    let _ = action_frame(
        &pointer_context,
        raw_input(vec![Event::PointerMoved(pointer_position)], 0.01),
        PlaylistStatusAction::RetrySave,
        true,
    );
    let _ = action_frame(
        &pointer_context,
        raw_input(vec![pointer_button(pointer_position, true)], 0.02),
        PlaylistStatusAction::RetrySave,
        true,
    );
    let (pointer_actions, _, _, _) = action_frame(
        &pointer_context,
        raw_input(vec![pointer_button(pointer_position, false)], 0.03),
        PlaylistStatusAction::RetrySave,
        true,
    );
    assert_eq!(
        pointer_actions,
        vec![super::super::actions::PlaylistAction::RetrySave]
    );

    for (action, key, expected) in [
        (
            progress_action,
            Key::Space,
            super::super::actions::PlaylistAction::CancelProgress(
                PlaylistProgressCancelScope::ManualAdd,
            ),
        ),
        (
            PlaylistStatusAction::CancelNavigation {
                origin_already_ended: false,
            },
            Key::Enter,
            super::super::actions::PlaylistAction::CancelNavigation,
        ),
    ] {
        let context = Context::default();
        let _ = action_frame(&context, RawInput::default(), action, true);
        let (_, _, tab_focused, _) = action_frame(&context, keyboard_input(Key::Tab), action, true);
        let (actions, _, still_focused, _) =
            action_frame(&context, keyboard_input(key), action, true);

        assert!(tab_focused);
        assert!(still_focused);
        assert_eq!(actions, vec![expected]);
    }
}

#[test]
fn actionless_status_button_is_disabled_before_residual_paint() {
    let context = Context::default();
    let action = PlaylistStatusAction::RetrySave;
    let (_, rect, _, enabled) = action_frame(&context, raw_input(Vec::new(), 0.0), action, false);
    let pointer_position = rect.center();
    let _ = action_frame(
        &context,
        raw_input(vec![Event::PointerMoved(pointer_position)], 0.01),
        action,
        false,
    );
    let _ = action_frame(
        &context,
        raw_input(vec![pointer_button(pointer_position, true)], 0.02),
        action,
        false,
    );
    let (actions, _, has_focus, still_enabled) = action_frame(
        &context,
        raw_input(vec![pointer_button(pointer_position, false)], 0.03),
        action,
        false,
    );

    assert!(!enabled);
    assert!(!still_enabled);
    assert!(!has_focus);
    assert!(actions.is_empty());
}

#[test]
fn authoritative_tombstone_keeps_go_current_focus_semantics() {
    let context = Context::default();
    let presentation = PlaylistStatusPresentation::tombstone_for_test();
    let mut state = PlaylistUiState::default();
    let mut output = PlaylistUiOutput::default();
    state.request_go_current(crate::playlist_runtime::PlaylistGoCurrentTarget::Tombstone);

    let _ = context.run_ui(raw_input(Vec::new(), 0.0), |ui| {
        render_presentation(
            ui,
            &presentation,
            PlaylistStatusRenderAccess::Authoritative {
                state: &mut state,
                output: &mut output,
            },
        );
    });

    assert!(state.take_go_current().is_none());
    assert!(context.memory(|memory| memory.focused().is_some()));
    assert!(output.take_actions().is_empty());
}

#[test]
fn residual_and_disabled_copies_are_actionless_and_do_not_mutate_authoritative_state() {
    let model = playlist_model(0, PlaylistLoadingView::Ready);
    let interaction = PlaylistInteractionModel {
        progress: Some(PlaylistProgressModel {
            stage: "Проверка файлов".into(),
            processed: 1,
            total: Some(2),
            cancel_scope: PlaylistProgressCancelScope::ManualAdd,
        }),
        ..PlaylistInteractionModel::default()
    };
    let state = PlaylistUiState::default();
    let mut output = PlaylistUiOutput::default();
    let authoritative_before = format!("{:?}", state.status);

    egui::__run_test_ui(|ui| {
        ui.disable();
        show_disabled_copy(ui, &model, &interaction);
    });

    assert_eq!(format!("{:?}", state.status), authoritative_before);
    assert!(output.take_actions().is_empty());

    // Measurement также получает actionless access и не трогает output/state.
    egui::__run_test_ui(|ui| {
        let presentation = PlaylistStatusPresentation::from_models(&model, &interaction)
            .expect("progress fixture must create a presentation");
        let _metrics = measure_status_layout(ui, &presentation);
    });
    assert_eq!(format!("{:?}", state.status), authoritative_before);
    assert!(output.take_actions().is_empty());
}
