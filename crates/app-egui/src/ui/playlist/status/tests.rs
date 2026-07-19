#![cfg(test)]

use std::path::PathBuf;
use std::time::Duration;

use egui::{Context, Event, Key, Modifiers, PointerButton, RawInput};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue,
};

use super::*;
use crate::playlist_runtime::{
    PlaylistInteractionModel, PlaylistSafeFeedback, PlaylistSafeFeedbackGeneration,
};
use crate::ui::playlist::status::presentation::{
    PlaylistStatusProblemIdentity, PlaylistStatusRetention,
};

/// Строит минимальную read model для status layout.
fn playlist_model(item_count: usize) -> PlaylistViewModel {
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
    PlaylistViewModel::for_queue_with_revision(&queue, 1)
}

/// Создаёт safe event через production presentation boundary.
fn safe_feedback(
    generation: u64,
    message: impl Into<std::sync::Arc<str>>,
) -> PlaylistInteractionModel {
    PlaylistInteractionModel {
        safe_feedback: Some(PlaylistSafeFeedback {
            generation: PlaylistSafeFeedbackGeneration(generation),
            message: message.into(),
        }),
        ..PlaylistInteractionModel::default()
    }
}

/// Возвращает полную высоту status owner-а в мгновенном reduced-motion режиме.
fn playlist_status_height(
    model: &PlaylistViewModel,
    interaction: &PlaylistInteractionModel,
) -> f32 {
    let mut height = 0.0;
    let context = Context::default();
    let _ = context.run_ui(raw_input(Vec::new(), 0.0), |ui| {
        // Production sidebar ограничен по ширине; sizing pass должен реально проверить wrapping.
        ui.set_width(220.0);
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

/// Строит одну event presentation для чистой математики transition.
fn event_presentation(generation: u64) -> PlaylistStatusPresentation {
    PlaylistStatusPresentation::from_rows(vec![PlaylistStatusRow::new(
        PlaylistStatusProblemIdentity::SafeFeedback(PlaylistSafeFeedbackGeneration(generation)),
        PlaylistStatusRetention::Event,
        "Не удалось выполнить действие",
        StatusTone::Warning,
        StatusRowKind::Normal,
        None,
    )])
    .expect("fixture contains one problem")
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
    let mut output = PlaylistUiOutput::default();
    let mut response_state = None;
    let _ = context.run_ui(input, |ui| {
        let mut access = if authoritative {
            PlaylistStatusRenderAccess::Authoritative {
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
fn empty_and_populated_queues_keep_only_the_top_separator() {
    let empty_model = playlist_model(0);
    let populated_model = playlist_model(30);
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
fn single_and_wrapped_problem_add_content_and_lower_separator() {
    let model = playlist_model(0);
    let single_line_height = playlist_status_height(&model, &safe_feedback(1, "Короткая ошибка"));
    let wrapped_height = playlist_status_height(
        &model,
        &safe_feedback(
            2,
            "Длинная безопасная строка ошибки должна переноситься внутри одной общей status-области без расширения sidebar ".repeat(12),
        ),
    );

    assert!(single_line_height > separator_height() * 2.0);
    assert!(
        wrapped_height > single_line_height,
        "wrapped={wrapped_height}, single={single_line_height}"
    );
}

#[test]
fn standard_slide_closes_after_deadline_and_reverses_for_new_problem() {
    let mut state = PlaylistStatusLifetimeState::default();
    let opened = state.advance(Some(event_presentation(1)), UiMotion::Standard, 0.0);
    let settled = state.advance(Some(event_presentation(1)), UiMotion::Standard, 0.180);
    let close_started = state.advance(Some(event_presentation(1)), UiMotion::Standard, 10.0);
    let closing = state.advance(Some(event_presentation(1)), UiMotion::Standard, 10.090);
    let reversed = state.advance(Some(event_presentation(2)), UiMotion::Standard, 10.090);
    let reopening = state.advance(Some(event_presentation(2)), UiMotion::Standard, 10.135);

    assert_eq!(opened.progress, 0.0);
    assert_eq!(settled.progress, 1.0);
    assert_eq!(close_started.progress, 1.0);
    assert!(closing.progress > 0.0 && closing.progress < 1.0);
    assert_eq!(reversed.progress, closing.progress);
    assert!(reopening.progress > reversed.progress);
}

#[test]
fn reduced_motion_closes_exactly_at_deadline_without_residual_snapshot() {
    let mut state = PlaylistStatusLifetimeState::default();

    let opened = state.advance(Some(event_presentation(1)), UiMotion::Reduced, 0.0);
    let before = state.advance(Some(event_presentation(1)), UiMotion::Reduced, 9.999);
    let closed = state.advance(Some(event_presentation(1)), UiMotion::Reduced, 10.0);

    assert_eq!(opened.progress, 1.0);
    assert!(before.presentation.is_some());
    assert_eq!(closed.progress, 0.0);
    assert!(closed.presentation.is_none());
    assert!(!closed.needs_repaint);
}

#[test]
fn settled_status_has_no_idle_repaint_and_schedules_only_nearest_deadline() {
    let mut state = PlaylistStatusLifetimeState::default();
    let first = state.advance(Some(event_presentation(1)), UiMotion::Standard, 2.0);
    let settled = state.advance(Some(event_presentation(1)), UiMotion::Standard, 2.180);

    assert!(first.needs_repaint);
    assert!(!settled.needs_repaint);
    assert_eq!(settled.repaint_after, Some(Duration::from_millis(9_820)));
}

#[test]
fn production_ui_requests_one_delayed_wake_at_the_problem_deadline() {
    let context = Context::default();
    let model = playlist_model(0);
    let interaction = safe_feedback(1, "Не удалось выполнить действие");
    let mut state = PlaylistUiState::default();
    let mut output = PlaylistUiOutput::default();

    // Первый headless frame может сам запросить immediate repaint для инициализации шрифтов.
    let _ = context.run_ui(raw_input(Vec::new(), 2.0), |ui| {
        show_status(
            ui,
            &model,
            &interaction,
            UiMotion::Reduced,
            &mut state,
            &mut output,
        );
    });
    let full_output = context.run_ui(raw_input(Vec::new(), 2.1), |ui| {
        show_status(
            ui,
            &model,
            &interaction,
            UiMotion::Reduced,
            &mut state,
            &mut output,
        );
    });
    let viewport_output = full_output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .expect("root viewport output exists");

    // Egui вычитает predicted_dt текущего frame-а, но не превращает wake в idle repaint.
    assert!(
        (Duration::from_millis(9_880)..=Duration::from_millis(9_900))
            .contains(&viewport_output.repaint_delay)
    );
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

    let navigation_action = PlaylistStatusAction::CancelNavigation {
        origin_already_ended: false,
    };
    let context = Context::default();
    let _ = action_frame(&context, RawInput::default(), navigation_action, true);
    let (_, _, tab_focused, _) =
        action_frame(&context, keyboard_input(Key::Tab), navigation_action, true);
    let (actions, _, still_focused, _) = action_frame(
        &context,
        keyboard_input(Key::Enter),
        navigation_action,
        true,
    );

    assert!(tab_focused);
    assert!(still_focused);
    assert_eq!(
        actions,
        vec![super::super::actions::PlaylistAction::CancelNavigation]
    );
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
fn disabled_copy_is_actionless_and_cannot_resurrect_expired_problem() {
    let model = playlist_model(0);
    let interaction = safe_feedback(1, "Не удалось выполнить действие");
    let mut state = PlaylistUiState::default();
    let mut output = PlaylistUiOutput::default();

    egui::__run_test_ui(|ui| {
        show_status(
            ui,
            &model,
            &interaction,
            UiMotion::Reduced,
            &mut state,
            &mut output,
        );
    });
    let _ = state.status.advance(None, UiMotion::Reduced, 10.0);
    let authoritative_before = format!("{:?}", state.status);

    egui::__run_test_ui(|ui| {
        ui.disable();
        show_disabled_copy(ui, &state);
    });

    assert_eq!(format!("{:?}", state.status), authoritative_before);
    assert!(output.take_actions().is_empty());
}
