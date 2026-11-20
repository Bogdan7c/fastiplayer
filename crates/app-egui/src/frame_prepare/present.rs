//! Видео-стадия, submit, present и трасса показанного кадра (планы `FullPass` и
//! `PresentReusingUi` сессии 04). Пропущенный кадр сюда не попадает.
//!
//! Порядок с сессии 06a: сначала буфер swapchain (под FIFO здесь главный поток ждёт
//! vsync), потом выбор видеокадра и субтитров, потом запись и показ. Раньше кадр выбирался
//! до ожидания и уходил на экран на ~16 мс старше. Если буфер не получен, кадр плеера не
//! забирается и остаётся в передаче для следующего показа.

use std::time::Instant;

use render_shell_api::{RenderFrameOutcome, RenderShell, SurfaceFrameAcquire};
use tracing::{error, warn};

use super::input_snapshot::AppFrameInputTimings;
use super::sequence::{FrameSequenceContract, FrameSequenceObserver, FrameSequenceStage};
use super::submit::{FrameSubmission, account_render_frame_outcome, submit_render_frame};
use super::timing::VideoPrepareTimings;
use super::timing::{
    AppRenderFrameTimings, FrameWorkTrace, SurfaceFrameCounters, log_render_frame_timings,
};
use super::ui_prepare::{PreparedUiFrame, UiPrepareTimings};
use super::{PreparedVideoFrame, prepare_video_frame, report_video_render_boundary_error};
use crate::state::{
    AppFrameContext, AppState, RejectedSwitchImpact, RendererVideoImportTarget,
    classify_rejected_switch,
};
use crate::telemetry::Telemetry;

/// Имя состояния выбора кадра в трассе, когда рисуется удержанный кадр прежнего медиа.
const MEDIA_SWITCH_HOLD_STATE: &str = "media_switch_hold";

/// Тайминги стадий до показа, которые нужны строке трассы показанного кадра.
pub(super) struct PresentedFrameTimings {
    pub(super) frame_start: Instant,
    pub(super) input_snapshot: AppFrameInputTimings,
    pub(super) ui_prepare: UiPrepareTimings,
    pub(super) frame_work: FrameWorkTrace,
}

/// Видео-стадия, submit, present и трасса — для планов `FullPass` и `PresentReusingUi`.
pub(super) fn present_frame_for_plan(
    telemetry: &Telemetry,
    renderer: &mut dyn RenderShell,
    app_state: &mut AppState,
    frame_context: &AppFrameContext,
    prepared_ui_frame: PreparedUiFrame,
    frame_sequence: &mut FrameSequenceContract,
    timings: PresentedFrameTimings,
) {
    let renderer_started_at = Instant::now();
    let surface = match renderer.acquire_surface_frame(prepared_ui_frame.screen) {
        SurfaceFrameAcquire::Acquired(surface) => surface,
        SurfaceFrameAcquire::Dropped(reason) => {
            log_frame_without_surface(
                telemetry,
                renderer,
                app_state,
                frame_context,
                RenderFrameOutcome::Dropped(reason),
                timings,
                renderer_started_at,
            );
            return;
        }
    };

    frame_sequence.reached(FrameSequenceStage::SurfaceAcquire);

    let stage_started_at = Instant::now();
    let video_selected_at = stage_started_at;
    let prepared_video_frame = prepare_video_frame(
        telemetry,
        app_state,
        renderer.video_frames(),
        frame_context.player_snapshot(),
    );
    frame_sequence.reached(FrameSequenceStage::MaterializerLookup);
    if let Err(fallback_failure) = app_state
        .apply_pending_dma_buf_runtime_fallback(RendererVideoImportTarget::for_renderer(renderer))
    {
        match fallback_failure.player_error {
            Some(player_error) => {
                warn!(error = %fallback_failure.error, "Runtime DMA-BUF layout recovery rejected");
                report_video_render_boundary_error(app_state, player_error);
            }
            // Отказ перехода Dynamic по «не успевает»: ошибка рендера нужна, только если
            // SW-декодер уже установлен, а переход не завершился (видео сломано).
            None => match classify_rejected_switch(&fallback_failure) {
                RejectedSwitchImpact::VideoKeepsPlaying => warn!(
                    error = %fallback_failure.error,
                    "Dynamic decode switch to software was rejected; staying on the current decoder"
                ),
                RejectedSwitchImpact::VideoBroken(player_error) => {
                    error!(
                        error = %fallback_failure.error,
                        "Dynamic decode switch failed after the software decoder was installed"
                    );
                    report_video_render_boundary_error(app_state, player_error);
                }
            },
        }
    }
    // Учёт смены медиа (замер решения 380): что показывает этот кадр — решается до submit,
    // засчитывается только если кадр правда ушёл на экран.
    let media_switch_content =
        app_state.media_switch_frame_content(prepared_video_frame.renderable_frame.as_ref());
    let mut video_prepare_timings = prepared_video_frame.timings;
    video_prepare_timings.total = stage_started_at.elapsed();
    let video_acquisition_state = prepared_video_frame.acquisition_state;
    let texture_view_lookup_state = prepared_video_frame.texture_view_lookup_state;

    frame_sequence.reached(FrameSequenceStage::RendererSubmit);
    let renderer_timing = submit_render_frame(
        telemetry,
        renderer,
        app_state,
        FrameSubmission {
            prepared_ui_frame,
            prepared_video_frame,
            surface,
            video_selected_at,
        },
    );
    // Стадия renderer-а — ожидание буфера + запись/показ; выбор кадра (video_prepare)
    // идёт между ними и считается своей стадией.
    let renderer_submit_elapsed = renderer_started_at
        .elapsed()
        .saturating_sub(video_prepare_timings.total);
    if renderer_timing.is_some() {
        app_state.note_media_switch_frame_presented(media_switch_content);
        // Только после настоящего показа: при свёрнутом окне/потерянной surface follow-up
        // крутил бы redraw вхолостую. Итог нужен лишь диагностике решения, не управлению.
        let _follow_up = app_state
            .request_follow_up_redraw_for_waiting_present_frame(frame_context.player_snapshot());
        // Рендер досчитывает картинку сам (таблица HDR → SDR в фоне GLES2): без просьбы на
        // паузе приложение спало бы, и готовая таблица не попала бы на экран.
        let _renderer_follow_up =
            app_state.request_follow_up_redraw_for_renderer(renderer.needs_follow_up_frame());
    }

    let frame_duration = timings.frame_start.elapsed();
    let frame_time_ms = frame_duration.as_secs_f64() * 1000.0;
    telemetry.update_fps(frame_time_ms);
    let surface_frame_counters = SurfaceFrameCounters {
        presented: telemetry.frames_presented_to_surface(),
        dropped: telemetry.surface_dropped_frames(),
    };
    log_render_frame_timings(
        frame_context,
        AppRenderFrameTimings {
            total: frame_duration,
            input_snapshot: timings.input_snapshot,
            ui_prepare: timings.ui_prepare,
            video_prepare: video_prepare_timings,
            renderer_submit: renderer_submit_elapsed,
            dma_buf_import_stats: renderer.video_frames().import_stats(),
            software_upload_stats: renderer.video_frames().software_upload_stats(),
            video_hot_path: renderer.hot_path_stats(),
        },
        timings.frame_work,
        video_acquisition_state,
        texture_view_lookup_state,
        surface_frame_counters,
        renderer_timing,
    );
}

/// Буфер swapchain не получен: учитываем пропуск и пишем трассу кадра без видео.
///
/// Видеокадр плеера не забирается (он покажется следующим кадром), но субтитры узнают режим
/// плеера, как и на любом кадре UI.
fn log_frame_without_surface(
    telemetry: &Telemetry,
    renderer: &mut dyn RenderShell,
    app_state: &mut AppState,
    frame_context: &AppFrameContext,
    dropped_outcome: RenderFrameOutcome,
    timings: PresentedFrameTimings,
    renderer_started_at: Instant,
) {
    app_state.observe_playback_for_subtitles();
    let renderer_timing = account_render_frame_outcome(telemetry, app_state, dropped_outcome, None);
    let renderer_submit_elapsed = renderer_started_at.elapsed();
    let not_prepared_video = PreparedVideoFrame::empty("surface_unavailable");
    let frame_duration = timings.frame_start.elapsed();
    telemetry.update_fps(frame_duration.as_secs_f64() * 1000.0);
    let surface_frame_counters = SurfaceFrameCounters {
        presented: telemetry.frames_presented_to_surface(),
        dropped: telemetry.surface_dropped_frames(),
    };
    log_render_frame_timings(
        frame_context,
        AppRenderFrameTimings {
            total: frame_duration,
            input_snapshot: timings.input_snapshot,
            ui_prepare: timings.ui_prepare,
            video_prepare: not_prepared_video.timings,
            renderer_submit: renderer_submit_elapsed,
            dma_buf_import_stats: renderer.video_frames().import_stats(),
            software_upload_stats: renderer.video_frames().software_upload_stats(),
            video_hot_path: renderer.hot_path_stats(),
        },
        timings.frame_work,
        not_prepared_video.acquisition_state,
        not_prepared_video.texture_view_lookup_state,
        surface_frame_counters,
        renderer_timing,
    );
}

/// Обычный путь выбора кадра не дал кадра (`acquisition_state` — его итог): на смене медиа окна
/// браузерного режима — удержанный кадр прежнего медиа (решение 380, владелец —
/// `state::present_frame_cache::media_switch_frame_hold`), иначе пустое поле, как раньше.
///
/// Это запасной путь, а не ворота перед выбором кадра: обычный путь (`acquire` → `materialize`)
/// идёт первым, и любой готовый кадр — нового медиа или ещё играющего прежнего — главнее
/// удержанного. Так конец удержания не зависит от того, успел ли снимок плеера сменить поколение.
pub(super) fn prepared_frame_without_current_video(
    app_state: &mut AppState,
    player_snapshot: &player_core::PlayerSnapshot,
    acquisition_state: &'static str,
    timings: VideoPrepareTimings,
) -> PreparedVideoFrame {
    match app_state.media_switch_frame_for_empty_video(player_snapshot) {
        Some(held_frame) => PreparedVideoFrame::ready(held_frame, MEDIA_SWITCH_HOLD_STATE)
            .with_diagnostics(MEDIA_SWITCH_HOLD_STATE, timings),
        None => {
            PreparedVideoFrame::empty(acquisition_state).with_diagnostics("not_requested", timings)
        }
    }
}
