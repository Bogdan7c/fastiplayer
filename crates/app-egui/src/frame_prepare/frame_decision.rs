//! Сбор фактов для плана кадра (cpu-parity, сессия 04).
//!
//! Здесь владельцы состояния опрашиваются один раз за пробуждение: триггеры (`AppState`),
//! запросы egui, анимации, таймеры UI-модели, явная UI-модель кадра, слот показа и shell
//! (можно ли повторить прежний интерфейс). Чистое решение — `frame_work_plan::plan_frame_work`.

use std::time::Instant;

use render_shell_api::{RenderScreenDescriptor, RenderShell};
use winit::window::Window;

use super::frame_work_plan::{FrameWorkPlan, PresentReasons, UiPassReasons, plan_frame_work};
use crate::settings_runtime::SettingsRuntime;
use crate::state::{
    AppFrameContext, AppState, PlaylistVisualKey, SettingsVisualFacts, UiFrameModel,
};

/// Итог сбора фактов: план и всё, что понадобится кадру дальше (модель для запоминания,
/// причины для трассы, признак страховочного пробуждения для счётчика).
pub(super) struct FrameDecision {
    pub(super) plan: FrameWorkPlan,
    pub(super) ui_reasons: UiPassReasons,
    pub(super) present_reasons: PresentReasons,
    /// UI-модель этого кадра: после полного прохода становится «последней нарисованной».
    pub(super) ui_frame_model: UiFrameModel,
    /// Это пробуждение инициировал страховочный таймер воспроизведения.
    pub(super) fallback_probe: bool,
}

/// Внешние факты кадра, которые знает только координатор `render_frame`.
pub(super) struct FrameDecisionInputs<'frame> {
    pub(super) window: &'frame Window,
    pub(super) renderer: &'frame dyn RenderShell,
    pub(super) frame_context: &'frame AppFrameContext,
    /// Сколько событий плеера вычитано в этом пробуждении.
    pub(super) worker_event_count: usize,
    pub(super) playlist: PlaylistVisualKey,
    pub(super) now: Instant,
}

/// Опрашивает владельцев состояния и решает, что делать с этим пробуждением.
///
/// Побочные эффекты: забираются накопленные триггеры `AppState` (они одноразовые) и
/// продвигаются таймеры уведомлений/inline-статуса (это делает и обычный проход).
pub(super) fn decide_frame_work(
    app_state: &mut AppState,
    settings_runtime: &mut SettingsRuntime,
    inputs: FrameDecisionInputs<'_>,
) -> FrameDecision {
    let now = inputs.now;
    let player_snapshot = inputs.frame_context.player_snapshot();
    let triggers = app_state.take_redraw_triggers();
    let egui_request = app_state.peek_egui_repaint_request();

    let settings_facts = SettingsVisualFacts {
        window_open: settings_runtime.is_settings_window_open(),
        ui_model_revision: settings_runtime.ui_model_revision(),
    };
    let ui_frame_model = app_state.observe_ui_frame_model(
        inputs.window,
        player_snapshot,
        inputs.playlist,
        settings_facts,
        now,
    );

    // Повторить прежний интерфейс можно только для того же размера экрана и только если
    // и app, и shell его помнят (после пересоздания renderer-а shell помнить не будет).
    let current_screen = RenderScreenDescriptor {
        size_in_pixels: ui_frame_model.window.inner_size,
        pixels_per_point: f32::from_bits(ui_frame_model.window.pixels_per_point_bits),
    };
    let retained_ui_unavailable = app_state
        .retained_ui_geometry()
        .is_none_or(|geometry| geometry.screen != current_screen)
        || !inputs.renderer.can_reuse_retained_ui(current_screen);

    let ui_reasons = UiPassReasons {
        window_input: triggers.window_input,
        player_events: inputs.worker_event_count > 0,
        worker_follow_up: app_state.has_pending_worker_redraw(),
        visible_mutation_wake: triggers.visible_mutation,
        egui_repaint_due: egui_request.is_due(now),
        animation_in_flight: app_state.ui_animation_in_flight()
            || app_state.toast_fade_in_flight(&ui_frame_model),
        ui_deadline_due: ui_deadline_is_due(app_state, settings_runtime, now),
        ui_model_changed: app_state.ui_frame_model_changed(&ui_frame_model),
        telemetry_refresh_due: app_state.telemetry_refresh_due(now),
        retained_ui_unavailable,
        forced: triggers.full_pass_forced,
    };
    let present_reasons = app_state.present_reasons(player_snapshot);

    FrameDecision {
        plan: plan_frame_work(ui_reasons, present_reasons),
        ui_reasons,
        present_reasons,
        ui_frame_model,
        fallback_probe: triggers.fallback_probe,
    }
}

/// Созрел ли хоть один таймер UI-модели, который до сессии 04 будил кадр через `WaitUntil`.
///
/// Дедлайны истечения (тост, inline-статус, автоскрытие) владельцы снимают сами при
/// продвижении; здесь они нужны, чтобы кадр, разбуженный именно таймером, не был пропущен,
/// если владелец ещё не успел изменить модель (например, Undo-отсчёт плейлиста).
fn ui_deadline_is_due(
    app_state: &AppState,
    settings_runtime: &SettingsRuntime,
    now: Instant,
) -> bool {
    [
        app_state.next_notification_wake_deadline(),
        app_state.fullscreen_chrome_wake_deadline(),
        app_state.timeline_inline_status_wake_deadline(),
        settings_runtime.next_sidebar_resize_deadline(),
    ]
    .into_iter()
    .flatten()
    .any(|deadline| deadline <= now)
}
