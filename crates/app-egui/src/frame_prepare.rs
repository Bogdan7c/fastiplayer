use std::time::{Duration, Instant};

use anyhow::Result;
use player_core::{
    LatencyCounterSnapshot, PlayerRenderError, PlayerRuntimeApplyGroup,
    PlayerRuntimeApplyGroupReport, PlayerRuntimeApplyReport, PlayerRuntimeApplyResult,
    PlayerRuntimeSettingsUpdate, PlayerSnapshot,
    PlayerTickResult, PlayerVideoDropReason, PlayerWorkerEvent, PreparedMedia,
};
use render_core::{
    RenderLiveApplyReport, RenderLiveSettings, RenderLiveSettingsAdapter, RenderLiveSettingsError,
    RenderLiveSettingsUpdate, RenderViewport,
};
use render_wgpu_shell::{RenderFrameDropReason, RenderFrameOutcome, RenderFrameTiming, Renderer};
use render_wgpu_video::{WgpuFrameTextureViewLookup, WgpuRenderableFrame};
use rustiplayer_settings::{AppRouteApplyResult, MediaServiceRuntimeSettingsUpdate};
use tracing::{debug, instrument, trace, warn};
use video_core::DecodedPixelFormat;
use winit::window::{ResizeDirection, Window};

use crate::redraw_pacing::RedrawPacing;
use crate::settings_runtime::{
    CommittedConfigSnapshot, SettingsRuntime, SettingsRuntimeReconfigureHost,
};
use crate::startup_media::{resolve_direct_media_startup_media, resolve_youtube_startup_media};
use crate::state::{
    ActiveMediaSource, AppFrameContext, AppState, AppUiRenderTimings, RenderablePresentFrame,
};
use crate::system_capabilities::probe_system_capabilities;
use crate::telemetry::{SeekDiscardReason, Telemetry, VideoDropReason, VideoFrameTelemetryEvent};
use crate::ui::window_chrome::{WindowChromeAction, WindowChromeResizeDirection};

/// Результат UI stage до входа в renderer/surface critical path.
struct PreparedUiFrame {
    /// Уже tessellated egui primitives для `egui-wgpu`.
    paint_jobs: Vec<egui::epaint::ClippedPrimitive>,

    /// Изменения egui texture atlas-а, которые renderer должен применить до pass-а.
    textures_delta: egui::TexturesDelta,

    /// Размер surface target-а и UI scale без раскрытия `egui-wgpu` наружу.
    screen: render_wgpu_shell::RenderScreenDescriptor,

    /// Физическая область, по которой renderer сохраняет video aspect ratio.
    video_viewport: RenderViewport,

    /// Физические области UI, из которых video pass должен быть исключён.
    video_exclusion_rects: Vec<RenderViewport>,

    /// Признак, что egui попросил следующий repaint.
    requested_repaint: bool,

    /// Visual settings actions, собранные в egui frame-е.
    settings_actions: Vec<crate::settings_ui::SettingsUiAction>,

    /// Window chrome actions, собранные в egui frame-е.
    window_chrome_actions: Vec<WindowChromeAction>,

    /// Подробная CPU-разбивка app-owned UI stage.
    timings: UiPrepareTimings,
}

/// Результат полного render frame-а с shell-level запросами от window chrome.
pub(crate) struct AppRenderFrameResult {
    /// Redraw pacing после рендера кадра.
    pub(crate) pacing: RedrawPacing,

    /// Пользователь запросил закрытие через кастомный titlebar.
    pub(crate) close_requested: bool,
}

/// Runtime adapter одного frame-а: render live preview + committed runtime owners.
struct FrameSettingsRuntimeAdapter<'frame> {
    /// App state владеет player worker и app-level media/source identity.
    app_state: &'frame mut AppState,

    /// Renderer владеет WGPU context и live render settings.
    renderer: &'frame mut Renderer,
}

/// Переводит egui coordinate в physical pixel с защитой от невалидного scale.
fn physical_pixel_floor(point: f32, pixels_per_point: f32) -> u32 {
    let scaled_point = point.max(0.0) * pixels_per_point;
    scaled_point.floor() as u32
}

/// Верхнюю/правую границу округляем вверх, чтобы viewport не терял крайний pixel.
fn physical_pixel_ceil(point: f32, pixels_per_point: f32) -> u32 {
    let scaled_point = point.max(0.0) * pixels_per_point;
    scaled_point.ceil() as u32
}

/// Возвращает безопасный UI scale для конвертации egui points в physical pixels.
fn safe_pixels_per_point(pixels_per_point: f32) -> f32 {
    if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    }
}

/// Конвертирует egui rect в renderer-neutral physical viewport без surface fallback-а.
fn raw_viewport_from_ui_rect(video_rect: egui::Rect, pixels_per_point: f32) -> RenderViewport {
    let safe_scale = safe_pixels_per_point(pixels_per_point);
    let min_x = physical_pixel_floor(video_rect.min.x, safe_scale);
    let min_y = physical_pixel_floor(video_rect.min.y, safe_scale);
    let max_x = physical_pixel_ceil(video_rect.max.x, safe_scale);
    let max_y = physical_pixel_ceil(video_rect.max.y, safe_scale);

    RenderViewport::new(
        min_x,
        min_y,
        max_x.saturating_sub(min_x),
        max_y.saturating_sub(min_y),
    )
}

/// Конвертирует app-owned video underlay rect в renderer-neutral physical viewport.
fn video_viewport_from_ui_rect(
    video_rect: egui::Rect,
    screen_size_in_pixels: [u32; 2],
    pixels_per_point: f32,
) -> RenderViewport {
    raw_viewport_from_ui_rect(video_rect, pixels_per_point)
        .clamp_to_surface(screen_size_in_pixels[0], screen_size_in_pixels[1])
}

/// Конвертирует UI exclusion rect в physical viewport; невалидный rect просто игнорируется.
fn video_exclusion_from_ui_rect(
    exclusion_rect: egui::Rect,
    screen_size_in_pixels: [u32; 2],
    pixels_per_point: f32,
) -> Option<RenderViewport> {
    let full_surface =
        RenderViewport::full_surface(screen_size_in_pixels[0], screen_size_in_pixels[1]);

    raw_viewport_from_ui_rect(exclusion_rect, pixels_per_point).intersection(full_surface)
}

impl RenderLiveSettingsAdapter for FrameSettingsRuntimeAdapter<'_> {
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.renderer.preview_live_settings(update)
    }

    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.renderer.commit_live_settings(settings)
    }

    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.renderer.rollback_live_settings(baseline)
    }
}

impl SettingsRuntimeReconfigureHost for FrameSettingsRuntimeAdapter<'_> {
    fn sync_committed_config_snapshot(&mut self, snapshot: CommittedConfigSnapshot) {
        self.app_state.sync_committed_config_snapshot(snapshot);
    }

    fn apply_player_runtime_settings(
        &mut self,
        update: PlayerRuntimeSettingsUpdate,
    ) -> PlayerRuntimeApplyResult {
        if update.decoder_thread_config.is_some() || update.video_backend.is_some() {
            let decoder_thread_config = update.decoder_thread_config.as_ref().map_or_else(
                || self.app_state.current_decoder_thread_config(),
                |decoder_update| decoder_update.decoder_thread_config,
            );
            if let Err(message) = self.app_state.rebuild_video_pipeline_with_decoder_config(
                decoder_thread_config,
                self.renderer.instance(),
                self.renderer.adapter(),
                self.renderer.device(),
                self.renderer.queue(),
            ) {
                return Ok(player_pipeline_rebuild_failure_report(&update, message));
            }
        }

        self.app_state.apply_player_runtime_settings(update)
    }

    fn apply_media_service_runtime_settings(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult {
        let Some(active_source) = self.app_state.active_media_source() else {
            return self.app_state.apply_media_service_runtime_settings(update);
        };
        let playback_snapshot = self.app_state.refresh_player_snapshot();
        let demux_config = self.app_state.demux_config_for_open();

        let rebuild_result = match active_source {
            ActiveMediaSource::LocalFile(_path) => {
                return self.app_state.apply_media_service_runtime_settings(update);
            }
            ActiveMediaSource::DirectMediaUrl(source_url) => {
                match resolve_direct_media_startup_media(
                    &source_url,
                    &update.network,
                    &demux_config,
                ) {
                    Ok(opened_media) => {
                        let source_label = opened_media.source_label().to_string();
                        let prepared_media = PreparedMedia::from_external_label(
                            source_label.clone(),
                            opened_media.into_demuxer(),
                        );
                        let media_loaded = self.app_state.load_prepared_direct_media(
                            source_url,
                            source_label,
                            prepared_media,
                        );
                        if media_loaded {
                            Ok(())
                        } else {
                            Err("direct media rebuild failed while sending media to worker"
                                .to_string())
                        }
                    }
                    Err(error) => Err(format!("direct media rebuild failed: {error}")),
                }
            }
            ActiveMediaSource::YouTubeUrl(source_url) => {
                let system_capabilities =
                    probe_system_capabilities(self.renderer.render_capabilities());
                match resolve_youtube_startup_media(
                    &source_url,
                    &update.network,
                    &update.youtube,
                    &demux_config,
                    &system_capabilities,
                ) {
                    Ok(streaming_media) => {
                        let media_loaded = self.app_state.load_youtube_demuxer(
                            source_url,
                            streaming_media.description,
                            streaming_media.demuxer,
                        );
                        if media_loaded {
                            Ok(())
                        } else {
                            Err(
                                "YouTube media rebuild failed while sending demuxer to worker"
                                    .to_string(),
                            )
                        }
                    }
                    Err(error) => Err(format!("YouTube media rebuild failed: {error}")),
                }
            }
        };

        match rebuild_result {
            Ok(()) => {
                self.app_state
                    .restore_playback_after_media_reconfigure(&playback_snapshot);
                AppRouteApplyResult::Applied
            }
            Err(message) => AppRouteApplyResult::Failed { message },
        }
    }
}

/// Строит typed player report, если app-owned pipeline rebuild не стартовал.
fn player_pipeline_rebuild_failure_report(
    update: &PlayerRuntimeSettingsUpdate,
    message: String,
) -> PlayerRuntimeApplyReport {
    let mut report = PlayerRuntimeApplyReport::empty();
    if let Some(decoder_update) = &update.decoder_thread_config {
        report.push(PlayerRuntimeApplyGroupReport::fatal(
            PlayerRuntimeApplyGroup::DecoderThreadConfig,
            decoder_update.affected_settings.clone(),
            message.clone(),
        ));
    }
    if let Some(backend_update) = &update.video_backend {
        report.push(PlayerRuntimeApplyGroupReport::fatal(
            PlayerRuntimeApplyGroup::VideoBackend,
            backend_update.affected_settings.clone(),
            message,
        ));
    }
    report
}

/// Video stage, подготовленный до финального `render_wgpu_shell::RenderFrameInput`.
struct PreparedVideoFrame {
    /// Lease и texture views живут вместе, чтобы `WgpuRenderableFrame` не пережил owner-а.
    renderable_frame: Option<RenderablePresentFrame>,

    /// Диагностическое имя acquisition state для tracing.
    acquisition_state: &'static str,

    /// Диагностическое имя texture-view lookup state.
    texture_view_lookup_state: &'static str,

    /// Подробная CPU-разбивка video prepare stage.
    timings: VideoPrepareTimings,
}

/// Tracing target для чистого включения render frame timings без packet debug шума.
const RENDER_FRAME_TIMING_TARGET: &str = "rustiplayer::render_frame_timing";

/// Fallback budget для startup/opening кадров, пока player snapshot ещё не измерил frame duration.
const DEFAULT_RENDER_FRAME_BUDGET: Duration = Duration::from_nanos(16_666_667);

/// Небольшой допуск, чтобы debug log показывал именно подозрительные кадры, а не шум таймера.
const SLOW_RENDER_FRAME_SLACK: Duration = Duration::from_millis(2);

/// Timings app-owned input/snapshot stage до UI подготовки.
#[derive(Debug, Clone, Copy, Default)]
struct AppFrameInputTimings {
    /// Полная длительность input/snapshot stage.
    total: Duration,

    /// Забор egui input из winit state.
    egui_input: Duration,

    /// Non-blocking drain worker events из player worker.
    worker_event_drain: Duration,

    /// Перенос worker tick/events в app telemetry/cache lifecycle.
    worker_event_record: Duration,

    /// Количество worker events, обработанных в этом кадре.
    worker_event_count: usize,

    /// Обновление `PlayerSnapshot` и фиксация render diagnostics.
    frame_context: Duration,

    /// Публикация snapshot в desktop integration.
    desktop_publish: Duration,
}

/// Timings app-owned UI подготовки до renderer boundary.
#[derive(Debug, Clone, Copy, Default)]
struct UiPrepareTimings {
    /// Полная длительность UI prepare stage.
    total: Duration,

    /// CPU-разбивка `AppState::render_ui`.
    app_ui: AppUiRenderTimings,

    /// Проверка repaint request после egui frame-а.
    repaint_query: Duration,

    /// Обработка platform output после egui frame-а.
    platform_output: Duration,

    /// Tessellation egui shapes в paint jobs.
    tessellate: Duration,

    /// Сбор surface size и pixels_per_point.
    screen_descriptor: Duration,
}

/// Timings app-owned video подготовки до renderer boundary.
#[derive(Debug, Clone, Copy, Default)]
struct VideoPrepareTimings {
    /// Полная длительность video prepare stage.
    total: Duration,

    /// Получение `PresentFrameLease` или reuse cached present frame.
    present_frame_acquire: Duration,

    /// Учёт repeated frame после acquisition.
    repeated_frame_accounting: Duration,

    /// Получение active WGPU materializer из app state.
    materializer_access: Duration,

    /// Lookup texture views по opaque frame handle.
    texture_view_lookup: Duration,

    /// Передача lock-wait/busy sample обратно в player diagnostics.
    resource_lookup_report: Duration,

    /// App-level действие после lookup: cache update, reuse, skip или error report.
    lookup_action: Duration,
}

/// Surface-level frame counters после renderer outcome handling текущего кадра.
#[derive(Debug, Clone, Copy, Default)]
struct SurfaceFrameCounters {
    /// Кадры, успешно отправленные в surface present path.
    presented: u64,

    /// Кадры, отброшенные на surface/render boundary.
    dropped: u64,
}

/// App-owned timing стадий до и вокруг вызова `render-wgpu-shell`.
#[derive(Debug, Clone, Copy)]
struct AppRenderFrameTimings {
    /// Полная длительность кадра на UI/render thread.
    total: Duration,

    /// Забор egui input, worker events, snapshot/context и desktop publication.
    input_snapshot: AppFrameInputTimings,

    /// Выполнение egui UI, platform output и tessellation.
    ui_prepare: UiPrepareTimings,

    /// Acquire present lease и materialization/lookup texture views.
    video_prepare: VideoPrepareTimings,

    /// Вызов renderer-owned submit/present path вместе с app-level outcome handling.
    renderer_submit: Duration,
}

impl PreparedVideoFrame {
    /// Создаёт video stage без renderable кадра.
    fn empty(acquisition_state: &'static str) -> Self {
        Self {
            renderable_frame: None,
            acquisition_state,
            texture_view_lookup_state: "not_requested",
            timings: VideoPrepareTimings::default(),
        }
    }

    /// Создаёт video stage с уже удерживаемым render lease-ом.
    fn ready(renderable_frame: RenderablePresentFrame, acquisition_state: &'static str) -> Self {
        Self {
            renderable_frame: Some(renderable_frame),
            acquisition_state,
            texture_view_lookup_state: "ready",
            timings: VideoPrepareTimings::default(),
        }
    }

    /// Добавляет диагностическую информацию, собранную в `prepare_video_frame`.
    fn with_diagnostics(
        mut self,
        texture_view_lookup_state: &'static str,
        timings: VideoPrepareTimings,
    ) -> Self {
        self.texture_view_lookup_state = texture_view_lookup_state;
        self.timings = timings;
        self
    }

    /// Собирает video boundary frame на время одного `render-wgpu-shell` call-а.
    fn render_input_video_frame(
        &self,
    ) -> Result<Option<WgpuRenderableFrame<'_>>, PlayerRenderError> {
        self.renderable_frame
            .as_ref()
            .map(|renderable_frame| {
                build_render_input_video_frame(renderable_frame, self.acquisition_state)
            })
            .transpose()
    }

    /// Отмечает, что подготовленный video frame реально попал в renderer submit.
    fn mark_submitted_to_renderer(&self) {
        if let Some(renderable_frame) = &self.renderable_frame {
            renderable_frame.present_frame.mark_submitted_to_renderer();
        }
    }
}

/// Typed форма texture-view lookup-а без GPU handles для проверки app-level контракта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureViewLookupKind {
    /// Texture views готовы для текущего present frame-а.
    Ready,

    /// Backend pool занят, render thread не должен ждать lock.
    Busy,

    /// Backend доступен, но resource для frame handle отсутствует.
    Missing,

    /// Resource descriptor существует, но текущий materializer его не поддерживает.
    Unsupported,

    /// Backend сообщил fatal/poisoned lookup state.
    Error,
}

/// App-level действие после texture-view lookup-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoFrameTexturePreparationAction {
    /// Рендерим текущий frame и обновляем cached renderable frame.
    RenderCurrentFrame,

    /// Рендерим cached previous frame из-за Busy, не делая blocking lookup.
    ReusePreviousFrameForTextureBusy {
        /// Нужно ли отдельно учесть repeated frame в shell telemetry.
        record_repeated_frame: bool,
    },

    /// Busy не дал безопасного cached frame-а; кадр не рендерится и ошибки нет.
    SkipVideoFrameForTextureBusy,

    /// Missing — это absent resource на render boundary, cache должен быть очищен.
    ReportMissingRenderResources,

    /// Unsupported — materializer не умеет текущий resource descriptor.
    ReportUnsupportedRenderResource,

    /// Error — fatal lookup на render boundary, cache должен быть очищен.
    ReportRenderResourceLookupFailure,
}

impl VideoFrameTexturePreparationAction {
    /// Возвращает `true`, если action должен инвалидировать cached renderable frame.
    const fn clears_cached_renderable_frame(self) -> bool {
        matches!(
            self,
            Self::ReportMissingRenderResources
                | Self::ReportUnsupportedRenderResource
                | Self::ReportRenderResourceLookupFailure
        )
    }
}

/// Кодирует fallback contract без GPU handles, чтобы tests проверяли именно boundary.
fn video_frame_texture_preparation_action(
    lookup_kind: TextureViewLookupKind,
    has_reusable_previous_frame: bool,
    acquisition_reused_previous_frame: bool,
) -> VideoFrameTexturePreparationAction {
    match lookup_kind {
        TextureViewLookupKind::Ready => VideoFrameTexturePreparationAction::RenderCurrentFrame,
        TextureViewLookupKind::Busy if has_reusable_previous_frame => {
            VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                record_repeated_frame: !acquisition_reused_previous_frame,
            }
        }
        TextureViewLookupKind::Busy => {
            VideoFrameTexturePreparationAction::SkipVideoFrameForTextureBusy
        }
        TextureViewLookupKind::Missing => {
            VideoFrameTexturePreparationAction::ReportMissingRenderResources
        }
        TextureViewLookupKind::Unsupported => {
            VideoFrameTexturePreparationAction::ReportUnsupportedRenderResource
        }
        TextureViewLookupKind::Error => {
            VideoFrameTexturePreparationAction::ReportRenderResourceLookupFailure
        }
    }
}

/// Переносит результат playback worker tick в shell telemetry.
fn record_player_tick_result(telemetry: &Telemetry, tick_result: &PlayerTickResult) {
    telemetry.record_packets(
        media_core::TrackKind::Audio,
        tick_result.dropped_seek_audio_preroll_packets,
    );

    for packet in &tick_result.demuxed_packets {
        telemetry.record_packet(packet.kind);

        if telemetry.packets_read() <= 50 {
            tracing::debug!(
                track_id = %packet.track_id,
                kind = ?packet.kind,
                pts_ms = packet.pts.as_millis(),
                raw_pts_units = ?packet.track_pts.map(|timestamp| timestamp.units.get()),
                raw_dts_units = ?packet.track_dts.map(|timestamp| timestamp.units.get()),
                size = packet.size,
                keyframe = ?packet.keyframe,
                "Packet"
            );
        }
    }

    for _ in 0..tick_result.decoded_video_frames {
        telemetry.record_video_frame_decoded();
    }

    for _ in 0..tick_result.video_frames_presented {
        telemetry.record_video_frame_presented();
    }

    for _ in 0..tick_result.video_frames_repeated {
        telemetry.record_video_frame_repeated();
    }

    for dropped_frame in &tick_result.dropped_video_frames {
        match map_video_frame_telemetry_event(dropped_frame.reason) {
            VideoFrameTelemetryEvent::PlaybackDrop(reason) => {
                telemetry.record_video_frame_dropped(reason);
            }
            VideoFrameTelemetryEvent::SeekDiscard(reason) => {
                telemetry.record_seek_discarded_frame(reason);
            }
        }
    }
}

/// Переносит worker event stream в shell telemetry и app-level lifecycle boundaries.
fn record_worker_events(
    telemetry: &Telemetry,
    app_state: &mut AppState,
    events: Vec<PlayerWorkerEvent>,
) {
    for event in events {
        match event {
            PlayerWorkerEvent::Tick(tick_result) => {
                record_player_tick_result(telemetry, &tick_result);
            }
            PlayerWorkerEvent::RenderError(_) => {
                app_state.clear_cached_present_frame_after_worker_render_error();
            }
            PlayerWorkerEvent::Player(player_event) => {
                app_state.handle_cached_present_frame_player_event(&player_event);
            }
        }
    }
}

/// Возвращает `true`, если surface drop означает разрыв lifecycle для cached texture.
fn render_drop_reason_invalidates_cached_present_frame(reason: RenderFrameDropReason) -> bool {
    matches!(
        reason,
        RenderFrameDropReason::SurfaceOccluded
            | RenderFrameDropReason::SurfaceLost
            | RenderFrameDropReason::SurfaceValidation
            | RenderFrameDropReason::SurfaceOutdatedRecoveryFailed
    )
}

/// Классифицирует core-причину удаления кадра для пользовательской telemetry.
fn map_video_frame_telemetry_event(reason: PlayerVideoDropReason) -> VideoFrameTelemetryEvent {
    match reason {
        PlayerVideoDropReason::Late => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::Late)
        }
        PlayerVideoDropReason::QueueOverflow => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::QueueOverflow)
        }
        PlayerVideoDropReason::Paused => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::Paused)
        }
        PlayerVideoDropReason::SeekPreroll => {
            VideoFrameTelemetryEvent::SeekDiscard(SeekDiscardReason::SeekPreroll)
        }
        PlayerVideoDropReason::StaleGeneration => {
            VideoFrameTelemetryEvent::SeekDiscard(SeekDiscardReason::StaleGeneration)
        }
        PlayerVideoDropReason::DecoderStarvation => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::DecoderStarvation)
        }
        PlayerVideoDropReason::RenderAcquisitionTimeout => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::Other)
        }
    }
}

/// Готовит egui output до входа в renderer/surface critical path.
fn prepare_ui_frame(
    window: &Window,
    app_state: &mut AppState,
    settings_runtime: &mut SettingsRuntime,
    egui_input: egui::RawInput,
    frame_context: &AppFrameContext,
) -> PreparedUiFrame {
    let ui_prepare_started_at = Instant::now();

    // Фоновый refresh dynamic options подбирается до сборки ui_model,
    // чтобы готовые списки попали в model этого же кадра.
    settings_runtime.poll_dynamic_options_refresh();

    // Анимация sidebar продвигается до сборки ui_model: visual hold должен знать,
    // видна ли ещё закрытая панель, чтобы field list не пропадал во время закрытия.
    let settings_panel_open = settings_runtime.is_settings_window_open();
    app_state.advance_sidebar_slide(settings_panel_open, Instant::now());
    settings_runtime
        .set_visual_hold(!settings_panel_open && app_state.sidebar_slide_is_animating());

    let settings_ui_model = settings_runtime.ui_model();
    let rendered_app_ui = app_state.render_ui(window, egui_input, frame_context, settings_ui_model);
    let crate::state::RenderedAppUi {
        full_output: egui_full_output,
        settings_actions,
        window_chrome_actions,
        video_viewport_rect,
        video_exclusion_rects,
        timings: app_ui_timings,
    } = rendered_app_ui;

    let stage_started_at = Instant::now();
    let requested_repaint = app_state.egui_ctx.has_requested_repaint();
    let repaint_query_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    app_state
        .egui_winit_state
        .handle_platform_output(window, egui_full_output.platform_output);
    let platform_output_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    let pixels_per_point = app_state.egui_ctx.pixels_per_point();
    let size = window.inner_size();
    let screen_size_in_pixels = [size.width.max(1), size.height.max(1)];
    let video_viewport =
        video_viewport_from_ui_rect(video_viewport_rect, screen_size_in_pixels, pixels_per_point);
    let video_exclusion_rects = video_exclusion_rects
        .into_iter()
        .filter_map(|exclusion_rect| {
            video_exclusion_from_ui_rect(exclusion_rect, screen_size_in_pixels, pixels_per_point)
        })
        .collect();
    let screen_descriptor_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    let paint_jobs = app_state
        .egui_ctx
        .tessellate(egui_full_output.shapes, pixels_per_point);
    let tessellate_elapsed = stage_started_at.elapsed();

    PreparedUiFrame {
        paint_jobs,
        textures_delta: egui_full_output.textures_delta,
        screen: render_wgpu_shell::RenderScreenDescriptor {
            size_in_pixels: screen_size_in_pixels,
            pixels_per_point,
        },
        video_viewport,
        video_exclusion_rects,
        requested_repaint,
        settings_actions,
        window_chrome_actions,
        timings: UiPrepareTimings {
            total: ui_prepare_started_at.elapsed(),
            app_ui: app_ui_timings,
            repaint_query: repaint_query_elapsed,
            platform_output: platform_output_elapsed,
            tessellate: tessellate_elapsed,
            screen_descriptor: screen_descriptor_elapsed,
        },
    }
}

/// Применяет window chrome actions, которые не владеют lifecycle закрытия приложения.
fn apply_window_chrome_actions(window: &Window, actions: Vec<WindowChromeAction>) -> bool {
    let mut close_requested = false;

    for action in actions {
        match action {
            WindowChromeAction::Minimize => window.set_minimized(true),
            WindowChromeAction::ToggleMaximize => window.set_maximized(!window.is_maximized()),
            WindowChromeAction::Close => close_requested = true,
            WindowChromeAction::StartDrag => {
                if let Err(error) = window.drag_window() {
                    warn!(error = %error, "Не удалось начать перетаскивание окна");
                }
            }
            WindowChromeAction::BeginResize(direction) => {
                if let Err(error) = window.drag_resize_window(winit_resize_direction(direction)) {
                    warn!(
                        error = %error,
                        ?direction,
                        "Не удалось начать resize окна"
                    );
                }
            }
        }
    }

    close_requested
}

/// Маппит visual resize direction в winit API на shell boundary.
fn winit_resize_direction(direction: WindowChromeResizeDirection) -> ResizeDirection {
    match direction {
        WindowChromeResizeDirection::North => ResizeDirection::North,
        WindowChromeResizeDirection::NorthEast => ResizeDirection::NorthEast,
        WindowChromeResizeDirection::East => ResizeDirection::East,
        WindowChromeResizeDirection::SouthEast => ResizeDirection::SouthEast,
        WindowChromeResizeDirection::South => ResizeDirection::South,
        WindowChromeResizeDirection::SouthWest => ResizeDirection::SouthWest,
        WindowChromeResizeDirection::West => ResizeDirection::West,
        WindowChromeResizeDirection::NorthWest => ResizeDirection::NorthWest,
    }
}

/// Готовит video lease и texture views без входа в swapchain acquisition.
fn prepare_video_frame(
    telemetry: &Telemetry,
    app_state: &mut AppState,
    player_snapshot: &PlayerSnapshot,
) -> PreparedVideoFrame {
    let video_prepare_started_at = Instant::now();
    let mut timings = VideoPrepareTimings::default();

    let stage_started_at = Instant::now();
    let present_frame_acquisition = app_state.acquire_present_frame_for_render(player_snapshot);
    timings.present_frame_acquire = stage_started_at.elapsed();
    let acquisition_state = present_frame_acquisition.metric_name();
    let acquisition_reused_previous_frame = present_frame_acquisition.reused_previous_frame();

    let stage_started_at = Instant::now();
    if acquisition_reused_previous_frame {
        telemetry.record_video_frame_repeated();
    }
    timings.repeated_frame_accounting = stage_started_at.elapsed();

    let Some(present_frame) = present_frame_acquisition.into_present_frame() else {
        timings.total = video_prepare_started_at.elapsed();
        return PreparedVideoFrame::empty(acquisition_state)
            .with_diagnostics("not_requested", timings);
    };

    let stage_started_at = Instant::now();
    let texture_view_materializer = app_state.wgpu_frame_materializer();
    timings.materializer_access = stage_started_at.elapsed();

    let Some(texture_view_materializer) = texture_view_materializer else {
        let stage_started_at = Instant::now();
        report_video_render_boundary_error(
            app_state,
            PlayerRenderError::missing_render_resources(&present_frame),
        );
        timings.lookup_action = stage_started_at.elapsed();
        timings.total = video_prepare_started_at.elapsed();
        return PreparedVideoFrame::empty(acquisition_state)
            .with_diagnostics("missing_materializer", timings);
    };

    let stage_started_at = Instant::now();
    let texture_view_lookup =
        texture_view_materializer.try_texture_view_lookup(&present_frame.frame);
    timings.texture_view_lookup = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    present_frame.report_resource_lookup_sample(
        texture_view_lookup.texture_pool_lock_wait(),
        texture_view_lookup.lookup_was_busy(),
    );
    timings.resource_lookup_report = stage_started_at.elapsed();

    // Эта развилка остаётся до renderer/surface critical path: Busy не ждёт backend lock,
    // а Missing/Error не превращаются в silent fallback.
    match texture_view_lookup {
        WgpuFrameTextureViewLookup::Ready { views, .. } => {
            let stage_started_at = Instant::now();
            debug_assert_eq!(
                video_frame_texture_preparation_action(
                    TextureViewLookupKind::Ready,
                    false,
                    acquisition_reused_previous_frame,
                ),
                VideoFrameTexturePreparationAction::RenderCurrentFrame
            );
            let current_renderable_frame = RenderablePresentFrame::new(present_frame, views);
            app_state.remember_renderable_present_frame(
                current_renderable_frame.clone(),
                player_snapshot,
            );
            timings.lookup_action = stage_started_at.elapsed();
            timings.total = video_prepare_started_at.elapsed();
            PreparedVideoFrame::ready(current_renderable_frame, acquisition_state)
                .with_diagnostics("ready", timings)
        }
        WgpuFrameTextureViewLookup::Busy { .. } => {
            let stage_started_at = Instant::now();
            let reusable_renderable_frame =
                app_state.reusable_renderable_frame_for_texture_busy(player_snapshot);
            let preparation_action = video_frame_texture_preparation_action(
                TextureViewLookupKind::Busy,
                reusable_renderable_frame.is_some(),
                acquisition_reused_previous_frame,
            );

            let prepared_video_frame = match (preparation_action, reusable_renderable_frame) {
                (
                    VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                        record_repeated_frame,
                    },
                    Some(reusable_renderable_frame),
                ) => {
                    if record_repeated_frame {
                        telemetry.record_video_frame_repeated();
                    }
                    app_state.report_render_resource_previous_frame_reuse();
                    PreparedVideoFrame::ready(
                        reusable_renderable_frame,
                        "texture_view_busy_previous_frame_reuse",
                    )
                }
                (VideoFrameTexturePreparationAction::SkipVideoFrameForTextureBusy, None) => {
                    PreparedVideoFrame::empty("texture_view_busy_no_reusable_frame")
                }
                (
                    VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                        record_repeated_frame,
                    },
                    None,
                ) => unreachable!(
                    "Busy reuse action requires a cached renderable frame; record_repeated_frame={record_repeated_frame}"
                ),
                (VideoFrameTexturePreparationAction::SkipVideoFrameForTextureBusy, Some(_)) => {
                    unreachable!("Busy skip action must not keep a reusable cached frame")
                }
                _ => unreachable!("Busy lookup must only produce Busy preparation actions"),
            };
            timings.lookup_action = stage_started_at.elapsed();
            timings.total = video_prepare_started_at.elapsed();
            prepared_video_frame.with_diagnostics("busy", timings)
        }
        WgpuFrameTextureViewLookup::Missing { .. } => {
            let stage_started_at = Instant::now();
            let preparation_action = video_frame_texture_preparation_action(
                TextureViewLookupKind::Missing,
                false,
                acquisition_reused_previous_frame,
            );
            debug_assert!(preparation_action.clears_cached_renderable_frame());
            report_video_render_boundary_error(
                app_state,
                PlayerRenderError::missing_render_resources(&present_frame),
            );
            timings.lookup_action = stage_started_at.elapsed();
            timings.total = video_prepare_started_at.elapsed();
            PreparedVideoFrame::empty(acquisition_state).with_diagnostics("missing", timings)
        }
        WgpuFrameTextureViewLookup::Unsupported { .. } => {
            let stage_started_at = Instant::now();
            let preparation_action = video_frame_texture_preparation_action(
                TextureViewLookupKind::Unsupported,
                false,
                acquisition_reused_previous_frame,
            );
            debug_assert!(preparation_action.clears_cached_renderable_frame());
            report_video_render_boundary_error(
                app_state,
                PlayerRenderError::render_resource_lookup_failed(&present_frame),
            );
            timings.lookup_action = stage_started_at.elapsed();
            timings.total = video_prepare_started_at.elapsed();
            PreparedVideoFrame::empty(acquisition_state).with_diagnostics("unsupported", timings)
        }
        WgpuFrameTextureViewLookup::Error { .. } => {
            let stage_started_at = Instant::now();
            let preparation_action = video_frame_texture_preparation_action(
                TextureViewLookupKind::Error,
                false,
                acquisition_reused_previous_frame,
            );
            debug_assert!(preparation_action.clears_cached_renderable_frame());
            report_video_render_boundary_error(
                app_state,
                PlayerRenderError::render_resource_lookup_failed(&present_frame),
            );
            timings.lookup_action = stage_started_at.elapsed();
            timings.total = video_prepare_started_at.elapsed();
            PreparedVideoFrame::empty(acquisition_state).with_diagnostics("error", timings)
        }
    }
}

/// Передаёт render boundary error в player и сбрасывает cached frame lease.
fn report_video_render_boundary_error(app_state: &mut AppState, error: PlayerRenderError) {
    app_state.clear_cached_present_frame_after_render_failure();
    app_state.report_render_error(error);
    tracing::debug!(
        acquisition = "render_error_reported",
        "Present frame acquisition ended with render boundary error"
    );
}

/// Строит renderer-facing video frame из prepared lease-а и texture views.
fn build_render_input_video_frame<'frame>(
    renderable_frame: &'frame RenderablePresentFrame,
    acquisition_state: &'static str,
) -> Result<WgpuRenderableFrame<'frame>, PlayerRenderError> {
    let present_frame = &renderable_frame.present_frame;
    let texture_views = &renderable_frame.texture_views;
    let frame_format = present_frame.frame.format();
    let frame_memory_path = present_frame.frame.memory_path();

    tracing::trace!(
        handle_id = present_frame.frame.resource_handle.0,
        pts_ms = present_frame.frame.pts.as_millis(),
        format = %frame_format,
        memory_path = %frame_memory_path,
        stale = present_frame.stale,
        acquisition = acquisition_state,
        "Present frame acquired from playback worker"
    );

    let boundary_frame = match frame_format {
        DecodedPixelFormat::Nv12 => match texture_views.dma_buf_views() {
            Some((y_view, uv_view)) => {
                WgpuRenderableFrame::from_decoded_nv12(&present_frame.frame, y_view, uv_view)
            }
            None => Err(anyhow::anyhow!(
                "NV12 decoded video surface requires DMA-BUF Y/UV texture views"
            )),
        },
        DecodedPixelFormat::P010 => match texture_views.dma_buf_views() {
            Some((y_view, uv_view)) => {
                WgpuRenderableFrame::from_decoded_p010(&present_frame.frame, y_view, uv_view)
            }
            None => Err(anyhow::anyhow!(
                "P010 decoded video surface requires DMA-BUF Y/UV texture views"
            )),
        },
        DecodedPixelFormat::Rgba8 => Err(anyhow::anyhow!(
            "RGBA8 decoded video surface is not a production zero-copy render path"
        )),
        host_planar_layout if host_planar_layout.is_host_planar() => {
            match texture_views.host_planar_views() {
                Some((y_view, u_view, v_view)) => WgpuRenderableFrame::from_decoded_host_yuv(
                    &present_frame.frame,
                    y_view,
                    u_view,
                    v_view,
                ),
                None => Err(anyhow::anyhow!(
                    "{} decoded video surface requires HostPlanar Y/U/V texture views",
                    host_planar_layout
                )),
            }
        }
        unsupported_layout => Err(anyhow::anyhow!(
            "{} decoded video surface is not a production zero-copy render path",
            unsupported_layout
        )),
    };

    boundary_frame.map_err(|error| {
        let message = format!(
            "WGPU renderable frame rejected decoded {} frame: {}",
            frame_format, error
        );
        tracing::error!(
            error = %error,
            format = %frame_format,
            memory_path = %frame_memory_path,
            "Failed to build WGPU renderable frame"
        );
        PlayerRenderError::unsupported_frame_format(present_frame, message)
    })
}

/// Превращает длительность в миллисекунды для числовых tracing fields.
fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Возвращает worst latency одной player diagnostics stage в миллисекундах.
fn latency_worst_ms(counter: LatencyCounterSnapshot) -> f64 {
    counter
        .worst
        .map(|sample| duration_ms(sample.duration))
        .unwrap_or_default()
}

/// Возвращает frame budget из player snapshot-а без привязки scheduler-а к FPS магии.
fn render_frame_budget(player_snapshot: &PlayerSnapshot) -> Duration {
    let frame_budget = player_snapshot.video_frame_duration_estimate;
    if frame_budget.is_zero() {
        DEFAULT_RENDER_FRAME_BUDGET
    } else {
        frame_budget
    }
}

/// Логирует подробную разбивку кадра: trace для всех кадров, debug только для slow frame.
fn log_render_frame_timings(
    player_snapshot: &PlayerSnapshot,
    app_timings: AppRenderFrameTimings,
    video_acquisition_state: &'static str,
    texture_view_lookup_state: &'static str,
    surface_frame_counters: SurfaceFrameCounters,
    renderer_timing: Option<RenderFrameTiming>,
) {
    let frame_budget = render_frame_budget(player_snapshot);
    let slow_frame_threshold = frame_budget + SLOW_RENDER_FRAME_SLACK;
    let slowest_renderer_stage = renderer_timing.map(|timing| timing.stages.slowest_stage());
    let input_timings = app_timings.input_snapshot;
    let ui_timings = app_timings.ui_prepare;
    let app_ui_timings = ui_timings.app_ui;
    let video_timings = app_timings.video_prepare;
    let diagnostics = &player_snapshot.diagnostics;
    let queues = diagnostics.queues;
    let texture_slots = queues.texture_slots;
    let decoder_control_channel = queues.decoder_control_channel;
    let publish_pressure = diagnostics.decoder_frame_publish_pressure;
    let latencies = diagnostics.worst_latencies;
    let render_resource_lock_wait = latencies.render_resource_lock_wait;
    let gpu_submit_present = latencies.gpu_submit_present;
    let release_acknowledgement = latencies.release_acknowledgement;
    let worker_scheduler = latencies.worker_scheduler;
    let demux_read = latencies.demux_read;
    let decoder_submit = latencies.decoder_submit;
    let decoder_event_drain = latencies.decoder_event_drain;
    let dma_buf_import = latencies.dma_buf_import;
    let frame_counters = player_snapshot.frame_counters;

    trace!(
        target: RENDER_FRAME_TIMING_TARGET,
        frame_total_ms = duration_ms(app_timings.total),
        frame_budget_ms = duration_ms(frame_budget),
        input_snapshot_ms = duration_ms(input_timings.total),
        ui_prepare_ms = duration_ms(ui_timings.total),
        video_prepare_ms = duration_ms(video_timings.total),
        renderer_submit_ms = duration_ms(app_timings.renderer_submit),
        renderer_elapsed_ms = renderer_timing
            .map(|timing| duration_ms(timing.renderer_elapsed))
            .unwrap_or_default(),
        surface_acquire_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.surface_acquire))
            .unwrap_or_default(),
        queue_submit_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.queue_submit))
            .unwrap_or_default(),
        device_poll_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.device_poll))
            .unwrap_or_default(),
        surface_present_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.surface_present))
            .unwrap_or_default(),
        "render frame stage timings"
    );

    if app_timings.total <= slow_frame_threshold {
        return;
    }

    debug!(
        target: RENDER_FRAME_TIMING_TARGET,
        frame_total_ms = duration_ms(app_timings.total),
        slow_threshold_ms = duration_ms(slow_frame_threshold),
        input_snapshot_ms = duration_ms(input_timings.total),
        input_egui_ms = duration_ms(input_timings.egui_input),
        input_worker_event_drain_ms = duration_ms(input_timings.worker_event_drain),
        input_worker_event_record_ms = duration_ms(input_timings.worker_event_record),
        input_worker_event_count = input_timings.worker_event_count,
        input_frame_context_ms = duration_ms(input_timings.frame_context),
        input_desktop_publish_ms = duration_ms(input_timings.desktop_publish),
        ui_prepare_ms = duration_ms(ui_timings.total),
        ui_app_total_ms = duration_ms(app_ui_timings.total),
        ui_pre_setup_ms = duration_ms(app_ui_timings.pre_ui_setup),
        ui_egui_run_ms = duration_ms(app_ui_timings.egui_run),
        ui_top_bar_ms = duration_ms(app_ui_timings.top_bar),
        ui_bottom_controls_ms = duration_ms(app_ui_timings.bottom_controls),
        ui_telemetry_panel_ms = duration_ms(app_ui_timings.telemetry_panel),
        ui_center_overlay_ms = duration_ms(app_ui_timings.center_overlay),
        ui_post_actions_ms = duration_ms(app_ui_timings.post_ui_actions),
        ui_telemetry_panel_visible = app_ui_timings.telemetry_panel_visible,
        ui_repaint_query_ms = duration_ms(ui_timings.repaint_query),
        ui_platform_output_ms = duration_ms(ui_timings.platform_output),
        ui_tessellate_ms = duration_ms(ui_timings.tessellate),
        ui_screen_descriptor_ms = duration_ms(ui_timings.screen_descriptor),
        video_prepare_ms = duration_ms(video_timings.total),
        video_present_acquire_ms = duration_ms(video_timings.present_frame_acquire),
        video_repeated_accounting_ms = duration_ms(video_timings.repeated_frame_accounting),
        video_materializer_access_ms = duration_ms(video_timings.materializer_access),
        video_texture_lookup_ms = duration_ms(video_timings.texture_view_lookup),
        video_resource_report_ms = duration_ms(video_timings.resource_lookup_report),
        video_lookup_action_ms = duration_ms(video_timings.lookup_action),
        video_acquisition_state = video_acquisition_state,
        texture_view_lookup_state = texture_view_lookup_state,
        renderer_submit_ms = duration_ms(app_timings.renderer_submit),
        renderer_elapsed_ms = renderer_timing
            .map(|timing| duration_ms(timing.renderer_elapsed))
            .unwrap_or_default(),
        egui_texture_update_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.egui_texture_update))
            .unwrap_or_default(),
        encoder_creation_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.encoder_creation))
            .unwrap_or_default(),
        egui_buffer_update_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.egui_buffer_update))
            .unwrap_or_default(),
        surface_acquire_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.surface_acquire))
            .unwrap_or_default(),
        surface_view_creation_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.surface_view_creation))
            .unwrap_or_default(),
        video_render_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.video_render))
            .unwrap_or_default(),
        egui_render_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.egui_render))
            .unwrap_or_default(),
        queue_submit_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.queue_submit))
            .unwrap_or_default(),
        device_poll_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.device_poll))
            .unwrap_or_default(),
        pre_present_notify_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.pre_present_notify))
            .unwrap_or_default(),
        surface_present_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.surface_present))
            .unwrap_or_default(),
        slowest_renderer_stage = slowest_renderer_stage
            .map(|stage| stage.name)
            .unwrap_or("none"),
        slowest_renderer_stage_ms = slowest_renderer_stage
            .map(|stage| duration_ms(stage.elapsed))
            .unwrap_or_default(),
        surface_presented_frames = surface_frame_counters.presented,
        surface_dropped_frames = surface_frame_counters.dropped,
        playback_presented_frames = frame_counters.presented,
        playback_dropped_frames = frame_counters.dropped,
        playback_repeated_frames = frame_counters.repeated,
        queue_pending_audio_packets = queues.pending_audio_packets,
        queue_pending_video_packets = queues.pending_video_packets,
        queue_present_depth = queues.present_queue_depth,
        queue_decoder_send_depth = queues.decoder_send_queue_depth,
        queue_decoder_in_flight_packets = queues.decoder_in_flight_packets,
        queue_decoder_ready_depth = queues.decoder_ready_queue_depth.unwrap_or_default(),
        active_render_leases = queues.active_render_leases,
        deferred_render_releases = queues.deferred_render_releases,
        texture_slots_capacity = texture_slots.map(|slots| slots.capacity).unwrap_or_default(),
        texture_slots_created = texture_slots.map(|slots| slots.slots).unwrap_or_default(),
        texture_slots_in_use = texture_slots.map(|slots| slots.in_use).unwrap_or_default(),
        texture_slots_free_surfaces = texture_slots
            .map(|slots| slots.free_surfaces)
            .unwrap_or_default(),
        texture_slots_waiting_gpu_completion = texture_slots
            .map(|slots| slots.waiting_gpu_completion)
            .unwrap_or_default(),
        texture_slots_waiting_decoder_reuse = texture_slots
            .map(|slots| slots.waiting_decoder_reuse)
            .unwrap_or_default(),
        texture_import_failures = texture_slots
            .map(|slots| slots.import_failures)
            .unwrap_or_default(),
        texture_imports_created = texture_slots
            .map(|slots| slots.imports_created)
            .unwrap_or_default(),
        texture_imports_reused = texture_slots
            .map(|slots| slots.imports_reused)
            .unwrap_or_default(),
        texture_imports_replaced = texture_slots
            .map(|slots| slots.imports_replaced)
            .unwrap_or_default(),
        render_resource_lock_busy_count = diagnostics.render_resource_lock_busy_count,
        render_resource_previous_frame_reuse_count = diagnostics
            .render_resource_previous_frame_reuse_count,
        render_resource_lock_wait_samples = render_resource_lock_wait.samples,
        render_resource_lock_wait_avg_ms = duration_ms(render_resource_lock_wait.average),
        render_resource_lock_wait_worst_ms = latency_worst_ms(render_resource_lock_wait),
        gpu_submit_present_samples = gpu_submit_present.samples,
        gpu_submit_present_avg_ms = duration_ms(gpu_submit_present.average),
        gpu_submit_present_worst_ms = latency_worst_ms(gpu_submit_present),
        release_ack_samples = release_acknowledgement.samples,
        release_ack_avg_ms = duration_ms(release_acknowledgement.average),
        release_ack_worst_ms = latency_worst_ms(release_acknowledgement),
        worker_scheduler_samples = worker_scheduler.samples,
        worker_scheduler_avg_ms = duration_ms(worker_scheduler.average),
        worker_scheduler_worst_ms = latency_worst_ms(worker_scheduler),
        demux_read_samples = demux_read.samples,
        demux_read_avg_ms = duration_ms(demux_read.average),
        demux_read_worst_ms = latency_worst_ms(demux_read),
        decoder_submit_samples = decoder_submit.samples,
        decoder_submit_avg_ms = duration_ms(decoder_submit.average),
        decoder_submit_worst_ms = latency_worst_ms(decoder_submit),
        decoder_event_drain_samples = decoder_event_drain.samples,
        decoder_event_drain_avg_ms = duration_ms(decoder_event_drain.average),
        decoder_event_drain_worst_ms = latency_worst_ms(decoder_event_drain),
        dma_buf_import_samples = dma_buf_import.samples,
        dma_buf_import_avg_ms = duration_ms(dma_buf_import.average),
        dma_buf_import_worst_ms = latency_worst_ms(dma_buf_import),
        decoded_frame_publish_channel_full_count = publish_pressure
            .frame_publish_channel_full_count,
        decoded_frame_publish_retry_count = publish_pressure.pending_publish_retry_count,
        decoded_frame_publish_max_ms = duration_ms(
            publish_pressure.max_decoded_frame_publish_latency,
        ),
        decoder_control_len = decoder_control_channel
            .map(|pressure| pressure.control_channel_len)
            .unwrap_or_default(),
        decoder_control_capacity = decoder_control_channel
            .map(|pressure| pressure.control_channel_capacity)
            .unwrap_or_default(),
        decoder_control_full_count = decoder_control_channel
            .map(|pressure| pressure.control_channel_full_count)
            .unwrap_or_default(),
        decoder_release_control_send_fail_count = decoder_control_channel
            .map(|pressure| pressure.release_control_send_fail_count)
            .unwrap_or_default(),
        decoder_flush_control_send_fail_count = decoder_control_channel
            .map(|pressure| pressure.flush_control_send_fail_count)
            .unwrap_or_default(),
        "slow render frame stage timings"
    );
}

/// Собирает финальный GPU input и передаёт кадр в renderer-owned present path.
fn submit_render_frame(
    telemetry: &Telemetry,
    window: &Window,
    renderer: &mut Renderer,
    app_state: &mut AppState,
    prepared_ui_frame: PreparedUiFrame,
    prepared_video_frame: PreparedVideoFrame,
) -> Option<RenderFrameTiming> {
    let video_frame = match prepared_video_frame.render_input_video_frame() {
        Ok(video_frame) => video_frame,
        Err(error) => {
            report_video_render_boundary_error(app_state, error);
            None
        }
    };
    let submitted_video_frame = video_frame.is_some();

    match renderer.render_frame(render_wgpu_shell::RenderFrameInput {
        window,
        video_frame: video_frame.as_ref(),
        egui_paint_jobs: prepared_ui_frame.paint_jobs,
        egui_textures_delta: prepared_ui_frame.textures_delta,
        screen: prepared_ui_frame.screen,
        video_viewport: prepared_ui_frame.video_viewport,
        video_exclusion_rects: prepared_ui_frame.video_exclusion_rects,
    }) {
        RenderFrameOutcome::Presented(timing) => {
            if submitted_video_frame {
                prepared_video_frame.mark_submitted_to_renderer();
            }
            telemetry.record_frame_presented_to_surface();
            app_state.report_gpu_submit_present_latency(timing.submit_present_elapsed);
            Some(timing)
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

/// Рендерит один полный кадр: видео + egui overlay.
///
/// Измеряет время кадра, обновляет телеметрию,
/// и вызывает renderer.render_frame().
#[instrument(skip(telemetry, window, renderer, app_state, settings_runtime))]
pub(crate) fn render_frame(
    telemetry: &Telemetry,
    window: &Window,
    renderer: &mut Renderer,
    app_state: &mut AppState,
    settings_runtime: &mut SettingsRuntime,
) -> AppRenderFrameResult {
    let frame_start = Instant::now();

    let input_snapshot_started_at = Instant::now();
    let stage_started_at = Instant::now();
    let egui_input = app_state.egui_winit_state.take_egui_input(window);
    let egui_input_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    let worker_events = app_state.drain_worker_events();
    let worker_event_count = worker_events.len();
    let worker_event_drain_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    record_worker_events(telemetry, app_state, worker_events);
    let worker_event_record_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    let frame_context = app_state.begin_frame_context(renderer.diagnostics());
    let frame_context_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    app_state.publish_desktop_snapshot(frame_context.player_snapshot());
    let desktop_publish_elapsed = stage_started_at.elapsed();

    let input_snapshot_timings = AppFrameInputTimings {
        total: input_snapshot_started_at.elapsed(),
        egui_input: egui_input_elapsed,
        worker_event_drain: worker_event_drain_elapsed,
        worker_event_record: worker_event_record_elapsed,
        worker_event_count,
        frame_context: frame_context_elapsed,
        desktop_publish: desktop_publish_elapsed,
    };

    let stage_started_at = Instant::now();
    let mut prepared_ui_frame = prepare_ui_frame(
        window,
        app_state,
        settings_runtime,
        egui_input,
        &frame_context,
    );
    let egui_requested_repaint = prepared_ui_frame.requested_repaint;
    let settings_actions = std::mem::take(&mut prepared_ui_frame.settings_actions);
    let window_chrome_actions = std::mem::take(&mut prepared_ui_frame.window_chrome_actions);
    let mut ui_prepare_timings = prepared_ui_frame.timings;
    ui_prepare_timings.total = stage_started_at.elapsed();

    let settings_action_requested_repaint = {
        let mut runtime_adapter = FrameSettingsRuntimeAdapter {
            app_state,
            renderer,
        };
        match settings_runtime
            .handle_ui_actions_with_runtime_adapter(settings_actions, &mut runtime_adapter)
        {
            Ok(requested_repaint) => requested_repaint,
            Err(error) => {
                settings_runtime
                    .report_runtime_error("Не удалось обработать действие settings UI", error);
                true
            }
        }
    };
    let chrome_close_requested = apply_window_chrome_actions(window, window_chrome_actions);

    let settings_preview_tick = match settings_runtime.apply_due_preview(renderer, Instant::now()) {
        Ok(tick) => tick,
        Err(error) => {
            settings_runtime.report_runtime_error("Не удалось применить live preview", error);
            crate::settings_runtime::SettingsPreviewTick::default()
        }
    };
    if let Some(repaint_after) = settings_preview_tick.repaint_after {
        app_state.egui_ctx.request_repaint_after(repaint_after);
    }

    let stage_started_at = Instant::now();
    let prepared_video_frame =
        prepare_video_frame(telemetry, app_state, frame_context.player_snapshot());
    let mut video_prepare_timings = prepared_video_frame.timings;
    video_prepare_timings.total = stage_started_at.elapsed();
    let video_acquisition_state = prepared_video_frame.acquisition_state;
    let texture_view_lookup_state = prepared_video_frame.texture_view_lookup_state;

    let stage_started_at = Instant::now();
    let renderer_timing = submit_render_frame(
        telemetry,
        window,
        renderer,
        app_state,
        prepared_ui_frame,
        prepared_video_frame,
    );
    let renderer_submit_elapsed = stage_started_at.elapsed();

    let frame_duration = frame_start.elapsed();
    let frame_time_ms = frame_duration.as_secs_f64() * 1000.0;
    telemetry.update_fps(frame_time_ms);
    let surface_frame_counters = SurfaceFrameCounters {
        presented: telemetry.frames_presented_to_surface(),
        dropped: telemetry.surface_dropped_frames(),
    };
    log_render_frame_timings(
        frame_context.player_snapshot(),
        AppRenderFrameTimings {
            total: frame_duration,
            input_snapshot: input_snapshot_timings,
            ui_prepare: ui_prepare_timings,
            video_prepare: video_prepare_timings,
            renderer_submit: renderer_submit_elapsed,
        },
        video_acquisition_state,
        texture_view_lookup_state,
        surface_frame_counters,
        renderer_timing,
    );

    AppRenderFrameResult {
        pacing: RedrawPacing::new(
            app_state.wants_continuous_redraw(),
            app_state.take_pending_worker_redraw()
                || egui_requested_repaint
                || settings_action_requested_repaint,
        ),
        close_requested: chrome_close_requested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use player_core::{PlayerRenderErrorKind, PlayerVideoFrameDrop};
    use render_wgpu_shell::{RenderFrameDropReason, RenderFrameFailure};

    #[test]
    fn video_viewport_from_ui_rect_converts_points_to_physical_pixels() {
        let video_rect =
            egui::Rect::from_min_max(egui::pos2(100.25, 50.25), egui::pos2(300.5, 250.5));

        let viewport = video_viewport_from_ui_rect(video_rect, [1000, 800], 2.0);

        assert_eq!(viewport, RenderViewport::new(200, 100, 401, 401));
    }

    #[test]
    fn invalid_video_viewport_from_ui_rect_defaults_to_full_surface() {
        let empty_rect = egui::Rect::from_min_max(egui::pos2(40.0, 40.0), egui::pos2(40.0, 80.0));

        let viewport = video_viewport_from_ui_rect(empty_rect, [640, 360], f32::NAN);

        assert_eq!(viewport, RenderViewport::full_surface(640, 360));
    }

    #[test]
    fn video_exclusion_from_ui_rect_converts_sidebar_to_physical_pixels() {
        let sidebar_rect =
            egui::Rect::from_min_max(egui::pos2(0.0, 64.0), egui::pos2(420.0, 640.0));

        let exclusion = video_exclusion_from_ui_rect(sidebar_rect, [1280, 720], 1.0);

        assert_eq!(exclusion, Some(RenderViewport::new(0, 64, 420, 576)));
    }

    #[test]
    fn invalid_video_exclusion_from_ui_rect_is_ignored() {
        let empty_rect = egui::Rect::from_min_max(egui::pos2(40.0, 40.0), egui::pos2(40.0, 80.0));

        let exclusion = video_exclusion_from_ui_rect(empty_rect, [640, 360], f32::NAN);

        assert_eq!(exclusion, None);
    }

    /// Проверяет, что renderer error становится fatal media error, а не silent fallback.
    #[test]
    fn render_failure_maps_to_fatal_render_device_error() {
        let failure = RenderFrameFailure::new("P010 HDR renderer rejected strict metadata");

        let error = PlayerRenderError::render_device_lost(format!(
            "Video render failed: {}",
            failure.message
        ))
        .to_player_error();

        assert_eq!(error.kind, player_core::PlayerErrorKind::RenderDeviceLost);
        assert!(
            error
                .message
                .contains("P010 HDR renderer rejected strict metadata")
        );
    }

    /// Проверяет, что ошибка renderer boundary не превращается в silent empty frame.
    #[test]
    fn render_boundary_failure_maps_to_fatal_render_format_error() {
        let error = PlayerRenderError {
            kind: player_core::PlayerRenderErrorKind::UnsupportedFrameFormat,
            render_generation: Some(4),
            frame_handle: Some(9),
            message: "WGPU renderable frame rejected decoded P010 frame".into(),
        }
        .to_player_error();

        assert_eq!(
            error.kind,
            player_core::PlayerErrorKind::UnsupportedRenderFormat
        );
        assert!(
            error
                .message
                .contains("WGPU renderable frame rejected decoded P010 frame")
        );
    }

    /// Проверяет, что Busy + valid cached frame рендерит previous frame без cache clear.
    #[test]
    fn texture_view_busy_with_valid_cached_frame_reuses_previous_frame() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Busy, true, false);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                record_repeated_frame: true
            }
        );
        assert!(!preparation_action.clears_cached_renderable_frame());
    }

    /// Проверяет, что Busy reuse не удваивает repeated telemetry для already-reused lease-а.
    #[test]
    fn texture_view_busy_reuse_does_not_double_count_already_repeated_frame() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Busy, true, true);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                record_repeated_frame: false
            }
        );
        assert!(!preparation_action.clears_cached_renderable_frame());
    }

    /// Проверяет, что Busy без safe previous frame-а пропускает video input без fatal error.
    #[test]
    fn texture_view_busy_without_cached_frame_skips_video_input_without_error() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Busy, false, false);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::SkipVideoFrameForTextureBusy
        );
        assert!(!preparation_action.clears_cached_renderable_frame());
    }

    /// Проверяет, что Missing — absent resource boundary error, а не silent fallback.
    #[test]
    fn texture_view_missing_reports_boundary_error_and_clears_cache() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Missing, true, false);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::ReportMissingRenderResources
        );
        assert!(preparation_action.clears_cached_renderable_frame());
        let render_error = PlayerRenderError {
            kind: PlayerRenderErrorKind::MissingRenderResources,
            render_generation: Some(7),
            frame_handle: Some(42),
            message: "missing texture views".to_string(),
        };
        assert_eq!(
            render_error.to_player_error().kind,
            player_core::PlayerErrorKind::UnsupportedRenderFormat
        );
    }

    /// Проверяет, что Unsupported — отдельный descriptor boundary outcome.
    #[test]
    fn texture_view_unsupported_reports_boundary_error_and_clears_cache() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Unsupported, true, false);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::ReportUnsupportedRenderResource
        );
        assert!(preparation_action.clears_cached_renderable_frame());
    }

    /// Проверяет, что Error — fatal lookup boundary error, а не Busy fallback.
    #[test]
    fn texture_view_error_reports_boundary_error_and_clears_cache() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Error, true, false);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::ReportRenderResourceLookupFailure
        );
        assert!(preparation_action.clears_cached_renderable_frame());
        let render_error = PlayerRenderError {
            kind: PlayerRenderErrorKind::RenderResourceLookupFailed,
            render_generation: Some(7),
            frame_handle: Some(42),
            message: "texture view lookup failed".to_string(),
        };
        assert_eq!(
            render_error.to_player_error().kind,
            player_core::PlayerErrorKind::UnsupportedRenderFormat
        );
    }

    /// Проверяет absent-resource ветку нового video preparation boundary.
    #[test]
    fn prepared_video_frame_without_lease_has_no_render_input() {
        let prepared_video_frame = PreparedVideoFrame::empty("no_frame_yet");

        let render_input_frame = prepared_video_frame
            .render_input_video_frame()
            .expect("empty prepared video frame must not fail");

        assert!(render_input_frame.is_none());
    }

    /// Проверяет, что renderer input заимствует lease/views у prepared stage.
    #[test]
    fn prepared_video_frame_render_input_is_borrowed_from_prepared_stage() {
        fn render_input_with_prepared_lifetime<'prepared>(
            prepared_video_frame: &'prepared PreparedVideoFrame,
        ) -> Result<Option<WgpuRenderableFrame<'prepared>>, PlayerRenderError> {
            prepared_video_frame.render_input_video_frame()
        }

        let prepared_video_frame = PreparedVideoFrame::empty("no_frame_yet");
        let render_input_frame = render_input_with_prepared_lifetime(&prepared_video_frame)
            .expect("empty prepared frame must not fail lifetime check");

        assert!(render_input_frame.is_none());
    }

    /// Проверяет, что cache чистится на lifecycle break, но не на transient timeout.
    #[test]
    fn surface_drop_reason_invalidates_cached_present_frame_only_on_lifecycle_break() {
        assert!(render_drop_reason_invalidates_cached_present_frame(
            RenderFrameDropReason::SurfaceLost
        ));
        assert!(render_drop_reason_invalidates_cached_present_frame(
            RenderFrameDropReason::SurfaceOutdatedRecoveryFailed
        ));
        assert!(render_drop_reason_invalidates_cached_present_frame(
            RenderFrameDropReason::SurfaceValidation
        ));
        assert!(render_drop_reason_invalidates_cached_present_frame(
            RenderFrameDropReason::SurfaceOccluded
        ));
        assert!(!render_drop_reason_invalidates_cached_present_frame(
            RenderFrameDropReason::SurfaceTimeout
        ));
    }

    /// Проверяет, что seek-discard причины не попадают в пользовательский счётчик drops.
    #[test]
    fn seek_reasons_map_to_seek_discarded_frames() {
        let telemetry = Telemetry::new();
        let tick_result = PlayerTickResult {
            dropped_video_frames: vec![
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(120),
                    reason: PlayerVideoDropReason::SeekPreroll,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(140),
                    reason: PlayerVideoDropReason::StaleGeneration,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(160),
                    reason: PlayerVideoDropReason::Late,
                },
            ],
            ..PlayerTickResult::default()
        };

        record_player_tick_result(&telemetry, &tick_result);

        assert_eq!(telemetry.video_frames_dropped(), 1);
        assert_eq!(telemetry.video_frames_late_dropped(), 1);
        assert_eq!(telemetry.video_frames_other_dropped(), 0);
        assert_eq!(telemetry.seek_discarded_frames(), 2);
        assert_eq!(telemetry.seek_preroll_discarded(), 1);
        assert_eq!(telemetry.stale_generation_discarded(), 1);
    }

    /// Проверяет, что playback причины не смешиваются с seek или surface taxonomy.
    #[test]
    fn playback_reasons_map_to_dedicated_playback_categories() {
        let telemetry = Telemetry::new();
        let tick_result = PlayerTickResult {
            dropped_video_frames: vec![
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(120),
                    reason: PlayerVideoDropReason::Late,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(140),
                    reason: PlayerVideoDropReason::QueueOverflow,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(160),
                    reason: PlayerVideoDropReason::Paused,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(180),
                    reason: PlayerVideoDropReason::DecoderStarvation,
                },
            ],
            ..PlayerTickResult::default()
        };

        record_player_tick_result(&telemetry, &tick_result);

        assert_eq!(telemetry.video_frames_dropped(), 4);
        assert_eq!(telemetry.playback_visible_drops(), 3);
        assert_eq!(telemetry.video_late_drops(), 1);
        assert_eq!(telemetry.video_queue_drops(), 1);
        assert_eq!(telemetry.video_pause_drops(), 1);
        assert_eq!(telemetry.video_decoder_starvation(), 1);
        assert_eq!(telemetry.video_other_drops(), 0);
        assert_eq!(telemetry.seek_discarded_frames(), 0);
        assert_eq!(telemetry.surface_dropped_frames(), 0);
    }

    /// Проверяет, что чистые seek-discard причины не становятся playback/render drop-ами.
    #[test]
    fn pure_seek_discard_reasons_do_not_count_as_playback_or_render_drops() {
        let telemetry = Telemetry::new();
        let tick_result = PlayerTickResult {
            dropped_video_frames: vec![
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(120),
                    reason: PlayerVideoDropReason::SeekPreroll,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(140),
                    reason: PlayerVideoDropReason::StaleGeneration,
                },
            ],
            ..PlayerTickResult::default()
        };

        record_player_tick_result(&telemetry, &tick_result);

        assert_eq!(telemetry.video_frames_dropped(), 0);
        assert_eq!(telemetry.video_frames_late_dropped(), 0);
        assert_eq!(telemetry.video_frames_queue_dropped(), 0);
        assert_eq!(telemetry.video_frames_pause_dropped(), 0);
        assert_eq!(telemetry.video_frames_other_dropped(), 0);
        assert_eq!(telemetry.dropped_frames(), 0);
        assert_eq!(telemetry.seek_discarded_frames(), 2);
        assert_eq!(telemetry.seek_preroll_discarded(), 1);
        assert_eq!(telemetry.stale_generation_discarded(), 1);
    }
}
