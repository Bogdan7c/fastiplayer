//! Renderer submit, mark-submitted и surface telemetry одного кадра.

use std::time::Instant;

use player_core::PlayerRenderError;
use render_shell_api::{
    AcquiredSurfaceFrame, RenderFrameDropReason, RenderFrameOutcome, RenderFrameTiming,
    RenderShell, SubtitleOverlayFrame, UiTextureFullSetReason,
};
use video_present_core::VideoPresentFrameIdentity;

use super::ui_prepare::PreparedUiFrame;
use super::{
    PreparedVideoFrame, render_drop_reason_invalidates_cached_present_frame,
    render_outcome_marks_video_submitted,
};
use crate::state::{AppState, SubmittedFrameForSubtitles};
use crate::telemetry::Telemetry;

/// Отдельный tracing target позволяет acceptance включить покадровое доказательство точечно.
const VIDEO_RENDER_ACCEPTANCE_TARGET: &str = "fastiplayer::video_render_acceptance";

/// Кадр, уходящий в renderer вместе с уже полученным буфером swapchain.
pub(super) struct FrameSubmission {
    /// Подготовленный интерфейс (свежий или удержанный).
    pub(super) prepared_ui_frame: PreparedUiFrame,
    /// Видеокадр, выбранный ПОСЛЕ получения буфера (сессия 06a).
    pub(super) prepared_video_frame: PreparedVideoFrame,
    /// Буфер swapchain от `RenderShell::acquire_surface_frame`.
    pub(super) surface: AcquiredSurfaceFrame,
    /// Момент выбора видеокадра — для трассы «выбор → показ».
    pub(super) video_selected_at: Instant,
}

/// Передаёт краткоживущие UI/video resources renderer-у и учитывает outcome.
pub(super) fn submit_render_frame(
    telemetry: &Telemetry,
    renderer: &mut dyn RenderShell,
    app_state: &mut AppState,
    submission: FrameSubmission,
) -> Option<RenderFrameTiming> {
    let FrameSubmission {
        prepared_ui_frame,
        prepared_video_frame,
        surface,
        video_selected_at,
    } = submission;
    // Несовпадение формата кадра и его ресурсов рендер отклоняет ещё в `materialize`
    // (`Unsupported`), поэтому здесь ключ либо есть, либо кадра нет.
    let video_frame = prepared_video_frame.render_input_video_frame();
    let submitted_video_frame = video_frame.is_some();
    let startup_frame_identity = prepared_video_frame.current_frame_identity();

    // Субтитры строятся по pts кадра, который прямо сейчас уходит в renderer. Overlay
    // заимствует `app_state` до конца `render_frame`; после него `app_state` снова свободен.
    // Режим плеера (игра, пауза, seek, scrub) и скорость доходят до субтитров на КАЖДОМ кадре
    // UI, даже без нового видеокадра: во время seek/scrub старый кадр остаётся без субтитров,
    // а цель уже известна, и поток-рисовальщик готовит её заранее.
    app_state.observe_playback_for_subtitles();
    // Изменения текстур забираются здесь, а не при UI-проходе: рендер мог быть заменён в
    // этом же кадре (Apply настроек), и полный набор для нового рендера уйдёт сразу.
    let egui_content = app_state.egui_content_for_renderer(prepared_ui_frame.egui_content);
    let subtitle_frame = video_frame.and_then(|frame_key| {
        SubmittedFrameForSubtitles::from_submission(
            startup_frame_identity,
            frame_key.decoded_frame(),
            prepared_ui_frame.video_viewport,
            prepared_ui_frame.screen.size_in_pixels,
        )
    });
    let subtitle_overlay = subtitle_frame.as_ref().and_then(|frame| {
        app_state
            .subtitle_overlay_for_submitted_frame(frame)
            .map(|placed| SubtitleOverlayFrame {
                overlay: placed.overlay,
                picture_rect: frame.picture_rect(),
                layer_offsets: placed.layer_offsets,
            })
    });

    let render_frame_outcome = renderer.render_frame(render_shell_api::RenderFrameInput {
        surface,
        video_frame,
        egui: egui_content,
        screen: prepared_ui_frame.screen,
        video_viewport: prepared_ui_frame.video_viewport,
        video_exclusion_rects: prepared_ui_frame.video_exclusion_rects,
        window_corner_mask: prepared_ui_frame.window_corner_mask,
        subtitle_overlay,
    });
    let video_was_submitted =
        render_outcome_marks_video_submitted(&render_frame_outcome, submitted_video_frame);
    if video_was_submitted {
        prepared_video_frame.mark_submitted_to_renderer();
        tracing::trace!(
            target: VIDEO_RENDER_ACCEPTANCE_TARGET,
            "video frame submitted to renderer"
        );
        if let Some(identity) = startup_frame_identity {
            // Acceptance различает новые PTS и повторный submit того же кадра.
            // Событие находится за Presented gate; UI redraw сам по себе его
            // не создаёт. Это surface handoff, а не доказательство scanout.
            // `selection_to_present_ms` — возраст выбранного кадра к моменту показа: после
            // сессии 06a кадр выбирается после ожидания буфера swapchain, а не до него.
            tracing::trace!(
                target: VIDEO_RENDER_ACCEPTANCE_TARGET,
                frame_pts_ns = ?identity.pts().as_nanos(),
                render_generation = identity.render_generation(),
                decoded_generation = identity.decoded_generation(),
                selection_to_present_ms = video_selected_at.elapsed().as_secs_f64() * 1000.0,
                "current video frame submitted to surface"
            );
        }
    }

    account_render_frame_outcome(
        telemetry,
        app_state,
        render_frame_outcome,
        video_was_submitted
            .then_some(startup_frame_identity)
            .flatten(),
    )
}

/// Учитывает итог кадра renderer-а: телеметрия surface, отказ удержанного UI, сброс кэша.
///
/// `submitted_frame_identity` — identity видеокадра, реально ушедшего в renderer (для
/// startup-доказательства первого показанного кадра); `None`, если видео не отправлялось.
pub(super) fn account_render_frame_outcome(
    telemetry: &Telemetry,
    app_state: &mut AppState,
    render_frame_outcome: RenderFrameOutcome,
    submitted_frame_identity: Option<VideoPresentFrameIdentity>,
) -> Option<RenderFrameTiming> {
    match render_frame_outcome {
        RenderFrameOutcome::Presented(timing) => {
            telemetry.record_frame_presented_to_surface();
            app_state.report_gpu_submit_present_latency(timing.submit_present_elapsed);
            if let Some(frame_identity) = submitted_frame_identity {
                app_state.note_startup_surface_frame_presented(frame_identity);
            }
            Some(timing)
        }
        RenderFrameOutcome::Dropped(RenderFrameDropReason::RetainedUiUnavailable) => {
            // Shell не смог повторить прежний интерфейс (нечего удерживать или другой размер):
            // это не потеря кадра surface-ом, а наш промах решения. Следующий кадр — полным
            // проходом и сразу (кадр видео ещё не показан).
            telemetry.record_retained_ui_refused();
            app_state.force_full_ui_pass();
            app_state.request_follow_up_redraw_after_retained_ui_refusal();
            None
        }
        RenderFrameOutcome::Dropped(RenderFrameDropReason::UiTexturesOutOfSync) => {
            // Рендер не знает текстуры, которую правит дельта (новый рендер, сбой учёта): это
            // не потеря кадра surface-ом. Следующий кадр — свежий проход с полным набором.
            app_state.request_full_ui_texture_set(
                UiTextureFullSetReason::RendererReportedMissingTexture,
            );
            app_state.force_full_ui_pass();
            app_state.request_follow_up_redraw_after_retained_ui_refusal();
            None
        }
        RenderFrameOutcome::Dropped(reason) => {
            telemetry.record_surface_dropped_frame();
            if render_drop_reason_invalidates_cached_present_frame(reason) {
                app_state.clear_cached_present_frame_after_surface_lifecycle_break();
            }
            None
        }
        RenderFrameOutcome::Failed(failure) => {
            telemetry.record_surface_dropped_frame();
            app_state.clear_cached_present_frame_after_render_failure();
            app_state.report_render_error(PlayerRenderError::render_device_lost(format!(
                "Video render failed: {}",
                failure.message
            )));
            None
        }
    }
}
