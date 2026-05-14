//! Playback tick и A/V scheduler.
//!
//! Этот модуль держит логику, которая раньше жила в `app-egui::main`:
//! чтение packets из demuxer, audio throttle, отправку video packets в decoder,
//! приём decoded frames, backpressure и выбор кадра для показа.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::{
    VideoCodec, VideoDecodeRequirement, Vp9MetadataSource, Vp9RequirementProbe,
    Vp9RequirementRejection, probe_vp9_packet_requirement, resolve_vp9_metadata,
};
use media_core::{TrackId, TrackKind};
use rustiplayer_config::AppConfig;
use tracing::{trace, warn};

use crate::{
    PendingAudioPacket, PendingVideoPacket, PipelineLatencyStage, PipelinePauseReason,
    PlaybackState, PlayerError, PlayerErrorKind, PlayerSession,
    session::vp9_requirement_needs_packet_refinement,
};

/// Контекст одного playback tick.
#[derive(Debug, Clone, Copy)]
pub struct PlayerTickContext {
    /// Монотонное время shell на момент tick.
    pub now: Instant,

    /// Настройки scheduler/backpressure для текущего tick.
    pub config: PlayerTickConfig,
}

impl PlayerTickContext {
    /// Создаёт tick context с production defaults.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            now,
            config: PlayerTickConfig::default(),
        }
    }

    /// Создаёт tick context с явно переданным конфигом.
    #[must_use]
    pub const fn with_config(now: Instant, config: PlayerTickConfig) -> Self {
        Self { now, config }
    }
}

/// Конфигурация playback tick, backpressure и A/V scheduler.
#[derive(Debug, Clone, Copy)]
pub struct PlayerTickConfig {
    /// Сколько container packets можно прочитать за один tick.
    pub max_demux_packets_per_tick: usize,

    /// Максимум decoded video frames в очереди presentation.
    pub max_video_present_queue: usize,

    /// Минимальный запас свободных texture slots перед отправкой новых packets в decoder.
    pub min_texture_slots_available_for_decode: usize,

    /// Максимум сырых video packets между demuxer и decoder thread.
    pub max_pending_video_packets: usize,

    /// Временный bounded лимит video packets, пока audio buffer догоняет low-watermark.
    pub max_pending_video_packets_during_audio_catchup: usize,

    /// Максимум video packets, отправляемых decoder thread за один tick.
    pub max_video_packets_sent_per_tick: usize,

    /// Максимум decoded frames, принимаемых из decoder thread за один tick.
    pub max_decoded_video_frames_drained_per_tick: usize,

    /// Максимальный decode-ahead относительно audio clock.
    pub max_video_decode_ahead: Duration,

    /// Уровень audio buffer, выше которого audio packets временно не декодируются.
    pub audio_buffer_high_water_mark_ms: f64,

    /// Уровень audio buffer, ниже которого demux может читать сквозь video backpressure.
    pub audio_demux_low_water_mark_ms: f64,

    /// Минимальный audio buffer перед переходом autoplay из `Buffering` в `Playing`.
    pub audio_preroll_target_ms: f64,

    /// Максимальное время ожидания seek commit gates.
    pub seek_commit_timeout: Duration,

    /// Максимальное время ожидания live preview seek gates.
    pub seek_preview_timeout: Duration,

    /// Минимальный audio buffer перед resume после seek.
    pub seek_resume_audio_min_buffer_ms: f64,

    /// Минимальный запас готовых video frames перед resume после seek.
    pub seek_resume_video_min_ready_frames: usize,

    /// Минимальная позиция audio clock, после которой stalled audio считается реальным.
    pub audio_stall_min_position: Duration,

    /// Длительность без движения audio clock, после которой звук считается stalled.
    pub audio_stall_timeout: Duration,

    /// Fallback delta позиции для media без audio clock.
    pub position_fallback_delta: Duration,

    /// Небольшой lead scheduler-а относительно audio clock в долях video frame.
    pub video_present_lead_frames: f64,

    /// Окно раннего показа sequential frame в долях video frame.
    pub video_present_window_frames: f64,

    /// Grace перед late-drop в долях video frame.
    pub video_late_drop_grace_frames: f64,
}

impl Default for PlayerTickConfig {
    /// Возвращает текущие MVP-лимиты, перенесённые из app layer в player-core.
    fn default() -> Self {
        Self {
            max_demux_packets_per_tick: 6,
            max_video_present_queue: 8,
            min_texture_slots_available_for_decode: 2,
            max_pending_video_packets: 8,
            max_pending_video_packets_during_audio_catchup: 240,
            max_video_packets_sent_per_tick: 2,
            max_decoded_video_frames_drained_per_tick: 2,
            max_video_decode_ahead: Duration::from_millis(500),
            audio_buffer_high_water_mark_ms: 200.0,
            audio_demux_low_water_mark_ms: 100.0,
            audio_preroll_target_ms: 50.0,
            seek_commit_timeout: Duration::from_millis(10_000),
            seek_preview_timeout: Duration::from_millis(100),
            seek_resume_audio_min_buffer_ms: 50.0,
            seek_resume_video_min_ready_frames: 3,
            audio_stall_min_position: Duration::from_millis(100),
            audio_stall_timeout: Duration::from_millis(250),
            position_fallback_delta: Duration::from_micros(16_667),
            video_present_lead_frames: 0.5,
            video_present_window_frames: 1.0,
            video_late_drop_grace_frames: 2.0,
        }
    }
}

impl From<&AppConfig> for PlayerTickConfig {
    /// Собирает runtime-лимиты playback из пользовательского TOML-config.
    ///
    /// Низкоуровневые scheduler knobs, которых ещё нет в публичной TOML-схеме,
    /// остаются на production defaults `player-core`. Пользовательские поля из
    /// `config` перекрывают те лимиты, которые уже зафиксированы в Phase 5.
    fn from(config: &AppConfig) -> Self {
        let defaults = Self::default();

        Self {
            max_video_present_queue: config.video.present_queue_frames,
            max_video_decode_ahead: Duration::from_millis(config.video.max_decode_ahead_ms),
            audio_buffer_high_water_mark_ms: config.audio.buffer_target_ms as f64,
            audio_demux_low_water_mark_ms: (config.audio.buffer_target_ms as f64 * 0.5)
                .max(config.player.seek.resume_audio_min_buffer_ms as f64),
            audio_preroll_target_ms: config.player.seek.resume_audio_min_buffer_ms as f64,
            seek_commit_timeout: Duration::from_millis(config.player.seek.commit_timeout_ms),
            seek_preview_timeout: Duration::from_millis(config.player.seek.live_preview_budget_ms),
            seek_resume_audio_min_buffer_ms: config.player.seek.resume_audio_min_buffer_ms as f64,
            seek_resume_video_min_ready_frames: config.player.seek.resume_video_min_ready_frames,
            ..defaults
        }
    }
}

/// Итог работы одного playback tick для shell-телеметрии.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerTickResult {
    /// Packets, прочитанные из demuxer за tick.
    pub demuxed_packets: Vec<PlayerTickPacket>,

    /// Количество decoded frames, принятых из decoder thread.
    pub decoded_video_frames: u64,

    /// Количество кадров, выбранных scheduler-ом для показа.
    pub video_frames_presented: u64,

    /// Количество tick-ов, где текущий кадр был повторён.
    pub video_frames_repeated: u64,

    /// Список кадров, удалённых scheduler/backpressure логикой.
    pub dropped_video_frames: Vec<PlayerVideoFrameDrop>,

    /// Список typed pipeline pauses за tick.
    pub pipeline_pauses: Vec<PlayerPipelinePause>,

    /// `true`, если demux чтение остановилось из-за backpressure.
    pub demux_backpressured: bool,
}

impl PlayerTickResult {
    /// Запоминает packet для внешней телеметрии без передачи codec bytes наружу.
    fn record_demuxed_packet(&mut self, packet: &media_core::Packet) {
        self.demuxed_packets.push(PlayerTickPacket {
            track_id: packet.track_id,
            kind: packet.kind,
            pts: packet.pts,
            size: packet.data.len(),
            byte_offset: packet.byte_offset,
            keyframe: packet.keyframe,
        });
    }

    /// Учитывает принятый decoded video frame.
    fn record_decoded_video_frame(&mut self) {
        self.decoded_video_frames = self.decoded_video_frames.saturating_add(1);
    }

    /// Учитывает кадр, выбранный для presentation.
    fn record_presented_video_frame(&mut self) {
        self.video_frames_presented = self.video_frames_presented.saturating_add(1);
    }

    /// Учитывает повтор текущего present frame.
    fn record_repeated_video_frame(&mut self) {
        self.video_frames_repeated = self.video_frames_repeated.saturating_add(1);
    }

    /// Учитывает удалённый video frame вместе с причиной.
    fn record_dropped_video_frame(&mut self, pts: Duration, reason: PlayerVideoDropReason) {
        self.dropped_video_frames
            .push(PlayerVideoFrameDrop { pts, reason });
    }

    /// Учитывает typed pipeline pause.
    fn record_pipeline_pause(&mut self, reason: PipelinePauseReason) {
        self.pipeline_pauses.push(PlayerPipelinePause { reason });
    }
}

/// Packet summary для shell-телеметрии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerTickPacket {
    /// ID media track, из которого пришёл packet.
    pub track_id: TrackId,

    /// Тип track: audio или video.
    pub kind: TrackKind,

    /// Presentation timestamp packet-а.
    pub pts: Duration,

    /// Размер codec payload в bytes.
    pub size: usize,

    /// Safe source byte offset для demux seek, если container adapter его сообщил.
    pub byte_offset: Option<u64>,

    /// Признак keyframe для video packets.
    pub keyframe: bool,
}

/// Public compatibility имя причины удаления video frame.
pub use crate::VideoDropReason as PlayerVideoDropReason;

/// Summary удалённого video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerVideoFrameDrop {
    /// Presentation timestamp удалённого кадра.
    pub pts: Duration,

    /// Причина удаления кадра.
    pub reason: PlayerVideoDropReason,
}

/// Summary pipeline pause-а внутри tick telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerPipelinePause {
    /// Typed причина pause.
    pub reason: PipelinePauseReason,
}

impl PlayerSession {
    /// Выполняет один playback tick.
    ///
    /// Shell вызывает этот метод один раз на redraw. Метод намеренно не рендерит
    /// и не знает про egui: он только продвигает media pipeline и возвращает
    /// компактный результат для телеметрии.
    #[must_use]
    pub fn tick(&mut self, tick_context: PlayerTickContext) -> PlayerTickResult {
        let mut tick_result = PlayerTickResult::default();

        self.update_position_for_tick(tick_context.config.position_fallback_delta);

        if self.is_demuxing_active() && self.pipeline.demuxer.is_some() {
            read_demux_packets(self, &tick_context.config, &mut tick_result);
            self.process_pending_audio_packets_with_buffer_limit(
                tick_context.config.audio_buffer_high_water_mark_ms,
            );
        }

        process_pending_video_packets(
            self,
            tick_context.now,
            &tick_context.config,
            &mut tick_result,
        );
        self.finish_seek_commit_if_ready(
            tick_context.now,
            tick_context.config.seek_commit_timeout,
            tick_context.config.seek_preview_timeout,
            tick_context.config.seek_resume_audio_min_buffer_ms,
            effective_seek_resume_video_min_ready_frames(&tick_context.config),
        );
        if let Err(error) =
            self.finish_autoplay_preroll_if_ready(tick_context.config.audio_preroll_target_ms)
        {
            self.mark_fatal_error(error);
        }

        tick_result
    }

    /// Обрабатывает pending audio packets до достижения high-water mark audio buffer.
    pub(crate) fn process_pending_audio_packets_with_buffer_limit(
        &mut self,
        high_water_mark_ms: f64,
    ) {
        let high_water_mark_ms = sanitize_audio_high_water_mark(high_water_mark_ms);

        if self.audio_buffer_level_ms().unwrap_or(0.0) > high_water_mark_ms {
            return;
        }

        while let Some(packet) = self.pipeline.pending_audio_packets.pop_front() {
            if self.audio_buffer_level_ms().unwrap_or(0.0) > high_water_mark_ms {
                self.pipeline.pending_audio_packets.push_front(packet);
                break;
            }

            self.process_audio_packet(
                packet.track_id,
                packet.pts,
                packet.generation,
                &packet.encoded_bytes,
            );
        }
    }

    /// Обновляет playback position один раз за tick.
    fn update_position_for_tick(&mut self, position_fallback_delta: Duration) {
        if self.playback_state() != PlaybackState::Playing {
            return;
        }

        if let Some(audio_secs) = self.audio_clock_secs() {
            if let Ok(audio_position) = Duration::try_from_secs_f64(audio_secs) {
                self.update_current_position(saturating_duration_add(
                    self.pipeline.media_clock_base,
                    audio_position,
                ));
            }
        } else {
            self.advance_position(position_fallback_delta);
        }
    }
}

/// Нормализует high-water mark, чтобы внешний некорректный config не ломал audio throttle.
fn sanitize_audio_high_water_mark(high_water_mark_ms: f64) -> f64 {
    if high_water_mark_ms.is_finite() && high_water_mark_ms > 0.0 {
        high_water_mark_ms
    } else {
        PlayerTickConfig::default().audio_buffer_high_water_mark_ms
    }
}

/// Нормализует low-water mark для audio catch-up demux.
fn sanitize_audio_demux_low_water_mark(low_water_mark_ms: f64) -> f64 {
    if low_water_mark_ms.is_finite() && low_water_mark_ms > 0.0 {
        low_water_mark_ms
    } else {
        PlayerTickConfig::default().audio_demux_low_water_mark_ms
    }
}

/// Возвращает bounded лимит video packets для audio catch-up режима.
fn audio_catchup_pending_video_limit(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .max_pending_video_packets_during_audio_catchup
        .max(tick_config.max_pending_video_packets)
}

/// Проверяет, нужно ли временно приоритизировать demux ради заполнения audio buffer.
fn audio_demux_catchup_needed(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    audio_demux_catchup_needed_for_level(
        session.pipeline.audio_track_id.is_some(),
        session.audio_buffer_level_ms(),
        tick_config.audio_demux_low_water_mark_ms,
    )
}

/// Чистая часть audio catch-up policy для unit-тестов без CPAL device.
fn audio_demux_catchup_needed_for_level(
    audio_track_selected: bool,
    audio_buffer_level_ms: Option<f64>,
    low_water_mark_ms: f64,
) -> bool {
    if !audio_track_selected {
        return false;
    }

    let Some(audio_buffer_level_ms) = audio_buffer_level_ms else {
        return false;
    };

    audio_buffer_level_ms.is_finite()
        && audio_buffer_level_ms < sanitize_audio_demux_low_water_mark(low_water_mark_ms)
}

/// Читает новые packets из demuxer в пределах бюджета текущего tick.
fn read_demux_packets(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
) {
    for _ in 0..tick_config.max_demux_packets_per_tick {
        let prioritize_audio_catchup = audio_demux_catchup_needed(session, tick_config);
        if prioritize_audio_catchup
            && session.pipeline.pending_video_packets.len() >= tick_config.max_pending_video_packets
        {
            trace!(
                pending_video_packets = session.pipeline.pending_video_packets.len(),
                catchup_video_packet_limit = audio_catchup_pending_video_limit(tick_config),
                audio_buffer_ms = session.audio_buffer_level_ms().unwrap_or(0.0),
                low_water_mark_ms =
                    sanitize_audio_demux_low_water_mark(tick_config.audio_demux_low_water_mark_ms),
                "Demux audio catch-up: reading through video pressure"
            );
        }

        if !can_read_next_demux_packet_with_audio_priority(
            session,
            tick_config,
            prioritize_audio_catchup,
        ) {
            tick_result.demux_backpressured = true;
            record_pipeline_pause(session, tick_result, PipelinePauseReason::DemuxBackpressure);
            trace!(
                pending_video_packets = session.pipeline.pending_video_packets.len(),
                queued_video_frames = session.pipeline.video_frame_queue.len(),
                "Demux backpressure: waiting for decoder/presentation"
            );
            break;
        }

        let demux_read_started_at = Instant::now();
        let packet_result = {
            let Some(demuxer) = session.pipeline.demuxer.as_mut() else {
                break;
            };
            demuxer.next_packet()
        };
        session.record_pipeline_latency(
            PipelineLatencyStage::DemuxRead,
            demux_read_started_at.elapsed(),
            None,
            None,
        );

        match packet_result {
            Ok(Some(packet)) => {
                tick_result.record_demuxed_packet(&packet);
                route_demuxed_packet(session, packet);
            }
            Ok(None) => {
                session.enter_eof_drain();
                break;
            }
            Err(error) => {
                tracing::warn!(error = %error, "Ошибка чтения packet");
                session.mark_fatal_error(PlayerError::new(
                    PlayerErrorKind::DemuxError,
                    format!("Ошибка чтения packet: {error}"),
                ));
                break;
            }
        }
    }
}

/// Перекладывает packet из demuxer в соответствующую pending queue.
fn route_demuxed_packet(session: &mut PlayerSession, packet: media_core::Packet) {
    let generation = session.pipeline.seek_generation;

    match packet.kind {
        TrackKind::Audio => {
            session
                .pipeline
                .pending_audio_packets
                .push_back(PendingAudioPacket::new(
                    packet.track_id,
                    packet.pts,
                    generation,
                    packet.data,
                ));
        }
        TrackKind::Video => {
            session
                .pipeline
                .pending_video_packets
                .push_back(PendingVideoPacket::new(
                    packet.track_id,
                    packet.pts,
                    generation,
                    packet.data,
                    packet.keyframe,
                ));
        }
    }
}

/// Проверяет demux admission с явно переданным audio-priority флагом.
fn can_read_next_demux_packet_with_audio_priority(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    prioritize_audio_catchup: bool,
) -> bool {
    if prioritize_audio_catchup {
        return session.pipeline.pending_video_packets.len()
            < audio_catchup_pending_video_limit(tick_config);
    }

    if !has_texture_capacity_for_decode(session, tick_config) {
        return false;
    }

    if session.pipeline.pending_video_packets.len() >= tick_config.max_pending_video_packets {
        return false;
    }

    available_video_present_slots(session, tick_config) > 0
}

/// Проверяет, можно ли отправить video packet в decoder thread без чрезмерного decode-ahead.
fn can_send_video_packet_to_decoder(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    packet_pts: Duration,
) -> bool {
    if !has_texture_capacity_for_decode(session, tick_config) {
        return false;
    }

    if session.pipeline.audio_track_id.is_none() || session.pipeline.audio_clock.is_none() {
        return true;
    }

    let audio_now = session.audio_clock_now();
    let media_audio_now = saturating_duration_add(session.pipeline.media_clock_base, audio_now);
    let packet_lead = packet_pts.saturating_sub(media_audio_now);

    packet_lead <= tick_config.max_video_decode_ahead
}

/// Проверяет запас texture slots для ещё одного decoded frame.
fn has_texture_capacity_for_decode(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    let Some(ref thread) = session.pipeline.video_decoder_thread else {
        return true;
    };

    let Some(stats) = thread.texture_pool_stats() else {
        return true;
    };

    let available_slots = stats.available_slots();
    if available_slots <= tick_config.min_texture_slots_available_for_decode {
        trace!(
            texture_slots = stats.slots,
            texture_in_use = stats.in_use,
            texture_capacity = stats.capacity,
            available_slots,
            reserve = tick_config.min_texture_slots_available_for_decode,
            "Video backpressure: waiting for texture slots"
        );
        return false;
    }

    true
}

/// Возвращает количество свободных мест в presentation queue.
fn available_video_present_slots(session: &PlayerSession, tick_config: &PlayerTickConfig) -> usize {
    video_present_queue_limit(tick_config).saturating_sub(session.pipeline.video_frame_queue.len())
}

/// Возвращает безопасный лимит presentation queue.
fn video_present_queue_limit(tick_config: &PlayerTickConfig) -> usize {
    tick_config.max_video_present_queue.max(1)
}

/// Возвращает достижимый video preroll для seek resume с учётом размера presentation queue.
fn effective_seek_resume_video_min_ready_frames(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .seek_resume_video_min_ready_frames
        .max(1)
        .min(video_present_queue_limit(tick_config).saturating_add(1))
}

/// Кладёт decoded frame в presentation queue, сохраняя фиксированный размер очереди.
fn enqueue_decoded_video_frame(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
    frame: video_core::DecodedFrame,
) {
    let queue_limit = video_present_queue_limit(tick_config);

    while session.pipeline.video_frame_queue.len() >= queue_limit {
        let Some(stale_frame) = session.pipeline.video_frame_queue.pop_front() else {
            break;
        };

        release_video_texture(session, stale_frame.texture_handle);
        record_video_drop(
            session,
            tick_result,
            stale_frame.pts,
            PlayerVideoDropReason::QueueOverflow,
        );
        tracing::debug!(
            pts_ms = stale_frame.pts.as_millis(),
            queue_limit,
            "Dropping oldest queued video frame before enqueue"
        );
    }

    session.pipeline.video_frame_queue.push_back(frame);
}

/// Забирает fatal decoder-thread error и переводит его в состояние player session.
fn drain_video_decoder_thread_error(session: &mut PlayerSession) -> bool {
    let mut fatal_decoder_error = None;

    if let Some(thread) = session.pipeline.video_decoder_thread.as_ref() {
        while let Some(error) = thread.try_recv_error() {
            fatal_decoder_error = Some(error);
        }
    }

    let Some(error) = fatal_decoder_error else {
        return false;
    };

    session.pipeline.pending_video_packets.clear();
    session.mark_fatal_error(player_error_from_decode_thread_error(&error));
    true
}

/// Забирает typed diagnostics events от decoder/backend boundary.
fn drain_video_decoder_thread_diagnostics(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) {
    loop {
        let diagnostic_event = session
            .pipeline
            .video_decoder_thread
            .as_ref()
            .and_then(|thread| thread.try_recv_diagnostic_event());
        let Some(event) = diagnostic_event else {
            break;
        };
        match event {
            video_core::VideoDecoderDiagnosticEvent::FrameDropped { pts, reason } => {
                let drop_reason = match reason {
                    video_core::VideoDecoderDropReason::ReadyQueueOverflow => {
                        PlayerVideoDropReason::QueueOverflow
                    }
                };
                record_video_drop(session, tick_result, pts, drop_reason);
            }
        }
    }
}

/// Мапит fail-closed ошибку decoder thread в player error model.
fn player_error_from_decode_thread_error(error: &video_vaapi::DecodeThreadError) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!("Video decoder thread failed: {}", error.message()),
    )
}

/// Забирает готовые кадры из decoder thread и кладёт их в presentation queue.
fn drain_decoded_video_frames(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
) {
    drain_video_decoder_thread_diagnostics(session, tick_result);

    if drain_video_decoder_thread_error(session) {
        return;
    }

    if session.pipeline.video_decoder_thread.is_none() {
        if !session.pipeline.pending_video_packets.is_empty() {
            tracing::warn!(
                count = session.pipeline.pending_video_packets.len(),
                "No video decoder thread — dropping video packets"
            );
        }
        while let Some(packet) = session.pipeline.pending_video_packets.pop_front() {
            record_video_drop(
                session,
                tick_result,
                packet.pts,
                PlayerVideoDropReason::DecoderStarvation,
            );
        }
        return;
    }

    let playback_can_present = session.can_present_video();
    let receive_budget = if playback_can_present {
        available_video_present_slots(session, tick_config)
            .min(tick_config.max_decoded_video_frames_drained_per_tick)
    } else {
        usize::MAX
    };

    if receive_budget == 0 {
        return;
    }

    let decoded_frames = {
        let Some(thread) = session.pipeline.video_decoder_thread.as_ref() else {
            return;
        };
        let mut decoded_frames = Vec::new();
        for _ in 0..receive_budget {
            let Some(frame) = thread.try_recv_frame() else {
                break;
            };
            decoded_frames.push(frame);
        }
        decoded_frames
    };

    if drain_video_decoder_thread_error(session) {
        for frame in decoded_frames {
            release_video_texture(session, frame.texture_handle);
        }
        return;
    }
    drain_video_decoder_thread_diagnostics(session, tick_result);

    for frame in decoded_frames {
        tracing::debug!(
            pts_ms = frame.pts.as_millis(),
            format = %frame.format,
            bit_depth = %frame.bit_depth,
            memory_path = %frame.memory_path,
            width = frame.width,
            height = frame.height,
            "Video frame decoded"
        );
        tick_result.record_decoded_video_frame();
        session.record_decoded_frame_diagnostics(&frame);

        if session.should_drop_decoded_frame_for_seek(frame.pts) {
            release_video_texture(session, frame.texture_handle);
            record_video_drop(
                session,
                tick_result,
                frame.pts,
                PlayerVideoDropReason::SeekPreroll,
            );
            tracing::debug!(
                pts_ms = frame.pts.as_millis(),
                "Dropping pre-roll video frame before seek target"
            );
            continue;
        }

        session.observe_video_frame_pts(frame.pts);

        if playback_can_present {
            enqueue_decoded_video_frame(session, tick_config, tick_result, frame);
        } else {
            release_video_texture(session, frame.texture_handle);
            record_video_drop(
                session,
                tick_result,
                frame.pts,
                PlayerVideoDropReason::Paused,
            );
            tracing::debug!("Dropping decoded frame received while playback is paused");
        }
    }
}

/// Отправляет ограниченное число pending video packets в decoder thread.
fn send_pending_video_packets_to_decoder(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
) {
    if session.pipeline.video_decoder_thread.is_none() {
        return;
    }

    let mut sent_packets = 0usize;

    while sent_packets < tick_config.max_video_packets_sent_per_tick {
        let available_slots = available_video_present_slots(session, tick_config);
        if available_slots == 0 || sent_packets >= available_slots {
            break;
        }

        let Some(packet) = session.pipeline.pending_video_packets.front() else {
            break;
        };
        let packet_track_id = packet.track_id;
        let packet_pts = packet.pts;
        let packet_generation = packet.generation;
        let packet_keyframe = packet.keyframe;
        let encoded_bytes = packet.encoded_bytes.clone();

        if packet_generation != session.pipeline.seek_generation {
            session.pipeline.pending_video_packets.pop_front();
            record_video_drop(
                session,
                tick_result,
                packet_pts,
                PlayerVideoDropReason::StaleGeneration,
            );
            continue;
        }

        if session.pipeline.video_track_id != Some(packet_track_id) {
            session.pipeline.pending_video_packets.pop_front();
            continue;
        }

        if !accept_video_packet_for_decoder_bootstrap(session, packet_keyframe, packet_pts) {
            session.pipeline.pending_video_packets.pop_front();
            record_video_drop(
                session,
                tick_result,
                packet_pts,
                PlayerVideoDropReason::SeekPreroll,
            );
            continue;
        }

        let packet_probe = PendingVideoPacketProbe {
            track_id: packet_track_id,
            encoded_bytes,
        };
        if !validate_pending_video_packet_before_decode(session, &packet_probe) {
            break;
        }

        if !can_send_video_packet_to_decoder(session, tick_config, packet_pts) {
            trace!(
                pts_ms = packet_pts.as_millis(),
                audio_ms = session.audio_clock_now().as_millis(),
                max_ahead_ms = tick_config.max_video_decode_ahead.as_millis(),
                "A/V sync: holding video packet to limit decode-ahead"
            );
            break;
        }

        let Some(packet) = session.pipeline.pending_video_packets.pop_front() else {
            break;
        };

        trace!(
            pts_ms = packet.pts.as_millis(),
            encoded_len = packet.encoded_bytes.len(),
            keyframe = packet.keyframe,
            "Sending video packet to decoder thread"
        );

        let resolved_color = session
            .pipeline
            .active_video_requirement
            .as_ref()
            .and_then(|requirement| requirement.color.clone());
        let decode_packet = video_vaapi::DecodePacket {
            track_id: packet.track_id,
            pts: packet.pts,
            encoded_bytes: packet.encoded_bytes,
            keyframe: packet.keyframe,
            resolved_color,
        };

        let Some(ref thread) = session.pipeline.video_decoder_thread else {
            break;
        };
        if let Err(error) = thread.send_packet(decode_packet) {
            tracing::warn!(error = %error, "Failed to send packet to decoder thread");
            session.mark_fatal_error(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!("Video decoder thread stopped before accepting packet: {error}"),
            ));
            break;
        }

        sent_packets += 1;
    }
}

/// Пропускает inter-frames, пока decoder после flush ждёт новый keyframe.
fn accept_video_packet_for_decoder_bootstrap(
    session: &mut PlayerSession,
    packet_keyframe: bool,
    packet_pts: Duration,
) -> bool {
    if !session.pipeline.video_decoder_needs_keyframe {
        return true;
    }

    if !packet_keyframe {
        trace!(
            pts_ms = packet_pts.as_millis(),
            "Dropping video packet until decoder receives post-flush keyframe"
        );
        return false;
    }

    session.pipeline.video_decoder_needs_keyframe = false;
    true
}

/// Минимальный view pending packet-а для bitstream capability validation.
struct PendingVideoPacketProbe {
    /// Track ID нужен, чтобы найти container codec.
    track_id: TrackId,

    /// Codec payload нужен VP9 parser-у для чтения profile из uncompressed header.
    encoded_bytes: Bytes,
}

/// Проверяет profile/format до отправки packet-а в hardware decoder.
fn validate_pending_video_packet_before_decode(
    session: &mut PlayerSession,
    packet: &PendingVideoPacketProbe,
) -> bool {
    if session
        .pipeline
        .active_video_requirement
        .as_ref()
        .is_some_and(|requirement| !vp9_requirement_needs_packet_refinement(requirement))
    {
        return true;
    }

    let requirement = match video_requirement_from_packet(session, packet) {
        Ok(Some(requirement)) => requirement,
        Ok(None) => return true,
        Err(error) => {
            warn!(error = %error, "Video stream rejected by packet requirement probe");
            session.mark_fatal_error(error);
            session.pipeline.pending_video_packets.clear();
            return false;
        }
    };

    match session.refine_active_video_requirement(requirement) {
        Ok(()) => true,
        Err(error) => {
            warn!(error = %error, "Video stream rejected before hardware decode");
            session.mark_fatal_error(error);
            session.pipeline.pending_video_packets.clear();
            false
        }
    }
}

/// Читает VP9 profile из uncompressed header и строит уточнённое requirement.
fn vp9_requirement_from_packet(
    packet_data: &[u8],
    container_source: Option<Vp9MetadataSource>,
) -> Result<Option<VideoDecodeRequirement>, PlayerError> {
    match probe_vp9_packet_requirement(packet_data) {
        Vp9RequirementProbe::Candidate(candidate) => Ok(Some(
            resolve_vp9_metadata(container_source, Some(candidate)).requirement,
        )),
        Vp9RequirementProbe::Rejected(rejection) => Err(player_error_from_vp9_rejection(rejection)),
        Vp9RequirementProbe::Recoverable(uncertainty) => {
            trace!(?uncertainty, "VP9 requirement probe skipped before decode");
            Ok(None)
        }
    }
}

/// Переводит VP9 parser-policy reject в player error без generic hardware wording.
fn player_error_from_vp9_rejection(rejection: Vp9RequirementRejection) -> PlayerError {
    let kind = match rejection {
        Vp9RequirementRejection::UnsupportedBitDepth(_) => {
            PlayerErrorKind::UnsupportedVideoBitDepth
        }
        Vp9RequirementRejection::UnsupportedChroma(_) => PlayerErrorKind::UnsupportedVideoChroma,
    };

    PlayerError::new(kind, rejection.user_message())
}

/// Возвращает codec-specific requirement probe для VP9 или generic codec requirement.
fn video_requirement_from_packet(
    session: &PlayerSession,
    packet: &PendingVideoPacketProbe,
) -> Result<Option<VideoDecodeRequirement>, PlayerError> {
    let Some(codec) = session.video_codec_for_track(packet.track_id) else {
        return Ok(None);
    };

    match codec {
        VideoCodec::Vp9 => vp9_requirement_from_packet(
            &packet.encoded_bytes,
            session.vp9_container_metadata_source_for_track(packet.track_id),
        ),
        other_codec => Ok(Some(VideoDecodeRequirement::new(other_codec))),
    }
}

/// Удаляет лишние кадры, если presentation queue стала больше безопасного лимита.
fn trim_video_present_queue(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
) {
    let queue_limit = video_present_queue_limit(tick_config);

    while session.pipeline.video_frame_queue.len() > queue_limit {
        let Some(frame) = session.pipeline.video_frame_queue.pop_front() else {
            break;
        };

        release_video_texture(session, frame.texture_handle);
        record_video_drop(
            session,
            tick_result,
            frame.pts,
            PlayerVideoDropReason::QueueOverflow,
        );
        tracing::debug!(
            pts_ms = frame.pts.as_millis(),
            "Dropping frame: queue overflow protection"
        );
    }
}

/// Добавляет duration без panic при переполнении.
fn saturating_duration_add(timestamp: Duration, offset: Duration) -> Duration {
    timestamp.checked_add(offset).unwrap_or(Duration::MAX)
}

/// Возвращает безопасный неотрицательный множитель для `Duration::mul_f64`.
fn finite_non_negative_factor(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        fallback
    }
}

/// Рассчитывает media time, под который выбираем frame для ближайшего present.
fn target_media_time_for_present(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    presentation_now: Duration,
) -> Duration {
    let lead_frames = finite_non_negative_factor(
        tick_config.video_present_lead_frames,
        PlayerTickConfig::default().video_present_lead_frames,
    );
    let present_lead = session
        .pipeline
        .video_frame_duration_estimate
        .mul_f64(lead_frames);

    saturating_duration_add(presentation_now, present_lead)
}

/// Возвращает clock, от которого scheduler выбирает следующий video frame.
fn presentation_clock_now(session: &PlayerSession, audio_now: Duration) -> Duration {
    if session.pipeline.audio_clock.is_some() {
        saturating_duration_add(session.pipeline.media_clock_base, audio_now)
    } else {
        session.snapshot().current_position
    }
}

/// Возвращает допустимое окно выбора кадра вокруг target media time.
fn video_present_window(session: &PlayerSession, tick_config: &PlayerTickConfig) -> Duration {
    let window_frames = finite_non_negative_factor(
        tick_config.video_present_window_frames,
        PlayerTickConfig::default().video_present_window_frames,
    );

    session
        .pipeline
        .video_frame_duration_estimate
        .mul_f64(window_frames)
}

/// Возвращает допустимое опоздание кадра перед forced catch-up drop.
fn video_late_drop_grace(session: &PlayerSession, tick_config: &PlayerTickConfig) -> Duration {
    let grace_frames = finite_non_negative_factor(
        tick_config.video_late_drop_grace_frames,
        PlayerTickConfig::default().video_late_drop_grace_frames,
    );

    session
        .pipeline
        .video_frame_duration_estimate
        .mul_f64(grace_frames)
}

/// Проверяет, нужно ли дропнуть первый queued frame как реально устаревший.
fn should_drop_front_frame_as_late(
    video_frame_queue: &VecDeque<video_core::DecodedFrame>,
    target_media_time: Duration,
    late_drop_grace: Duration,
) -> bool {
    let Some(front_frame) = video_frame_queue.front() else {
        return false;
    };
    let Some(next_frame) = video_frame_queue.get(1) else {
        return false;
    };

    let latest_front_pts = saturating_duration_add(front_frame.pts, late_drop_grace);
    if target_media_time <= latest_front_pts {
        return false;
    }

    next_frame.pts <= target_media_time
}

/// Проверяет, слишком ли рано показывать первый queued frame.
fn should_wait_for_front_frame(
    frame_pts: Duration,
    target_media_time: Duration,
    present_window: Duration,
) -> bool {
    frame_pts > saturating_duration_add(target_media_time, present_window)
}

/// Освобождает texture handle через decoder thread, если он ещё существует.
fn release_video_texture(
    session: &mut PlayerSession,
    texture_handle: video_core::FrameTextureHandle,
) {
    session.release_video_texture(texture_handle);
}

/// Удаляет первый queued frame и записывает причину drop.
fn drop_front_queued_video_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    reason: PlayerVideoDropReason,
) -> bool {
    let Some(frame) = session.pipeline.video_frame_queue.pop_front() else {
        return false;
    };

    let frame_pts = frame.pts;
    release_video_texture(session, frame.texture_handle);
    record_video_drop(session, tick_result, frame_pts, reason);
    tracing::debug!(
        pts_ms = frame_pts.as_millis(),
        ?reason,
        "Dropping queued video frame"
    );
    true
}

/// Делает первый queued frame текущим present frame.
fn present_front_queued_video_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) -> bool {
    let Some(frame) = session.pipeline.video_frame_queue.pop_front() else {
        return false;
    };

    if let Some(old_frame) = session.pipeline.present_video_frame.take() {
        release_video_texture(session, old_frame.texture_handle);
    }

    tracing::debug!(
        pts_ms = frame.pts.as_millis(),
        "Presenting scheduled video frame"
    );
    let frame_pts = frame.pts;
    session.pipeline.present_video_frame = Some(frame);
    session.note_presented_frame_for_seek(frame_pts);
    tick_result.record_presented_video_frame();
    true
}

/// Повторно показывает текущий кадр и учитывает это в telemetry result.
fn repeat_present_video_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    pause_reason: Option<PipelinePauseReason>,
) {
    if session.pipeline.present_video_frame.is_some() {
        tick_result.record_repeated_video_frame();
    }
    if session.pipeline.video_track_id.is_some()
        && session.playback_state() == PlaybackState::Playing
        && let Some(pause_reason) = pause_reason
    {
        record_pipeline_pause(session, tick_result, pause_reason);
    }
}

/// Обрабатывает pending video packets: приём кадров, backpressure и A/V sync.
fn process_pending_video_packets(
    session: &mut PlayerSession,
    now: Instant,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
) {
    drain_decoded_video_frames(session, tick_config, tick_result);

    let playback_can_present = session.can_present_video();
    if !playback_can_present {
        return;
    }

    if session.is_demuxing_active() {
        send_pending_video_packets_to_decoder(session, tick_config, tick_result);
    }

    trim_video_present_queue(session, tick_config, tick_result);
    let scheduler_started_at = Instant::now();

    if !session.pipeline.video_frame_queue.is_empty() {
        tracing::debug!(
            queue_len = session.pipeline.video_frame_queue.len(),
            "A/V sync: processing frame queue"
        );
    }

    let audio_now = session.audio_clock_now();
    if audio_now != session.pipeline.last_audio_clock {
        session.pipeline.last_audio_clock = audio_now;
        session.pipeline.last_audio_clock_change_at = now;
    }

    let audio_stall_elapsed =
        now.saturating_duration_since(session.pipeline.last_audio_clock_change_at);
    let audio_stalled = audio_now >= tick_config.audio_stall_min_position
        && audio_stall_elapsed >= tick_config.audio_stall_timeout;

    if audio_stalled {
        tracing::debug!(
            audio_ms = audio_now.as_secs_f64() * 1000.0,
            stalled_ms = audio_stall_elapsed.as_millis(),
            queue_len = session.pipeline.video_frame_queue.len(),
            "A/V sync: audio stalled"
        );

        if !present_front_queued_video_frame(session, tick_result) {
            repeat_present_video_frame(
                session,
                tick_result,
                Some(PipelinePauseReason::DecoderStarvation),
            );
        }
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    }

    let presentation_now = presentation_clock_now(session, audio_now);
    let target_media_time = target_media_time_for_present(session, tick_config, presentation_now);
    let present_window = video_present_window(session, tick_config);
    let late_drop_grace = video_late_drop_grace(session, tick_config);

    while should_drop_front_frame_as_late(
        &session.pipeline.video_frame_queue,
        target_media_time,
        late_drop_grace,
    ) {
        if !drop_front_queued_video_frame(session, tick_result, PlayerVideoDropReason::Late) {
            break;
        }
    }

    let Some(frame) = session.pipeline.video_frame_queue.front() else {
        repeat_present_video_frame(
            session,
            tick_result,
            Some(PipelinePauseReason::DecoderStarvation),
        );
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    };

    let diff_ms = frame.pts.as_secs_f64() * 1000.0 - target_media_time.as_secs_f64() * 1000.0;
    if should_wait_for_front_frame(frame.pts, target_media_time, present_window) {
        trace!(
            pts_ms = frame.pts.as_millis(),
            target_ms = target_media_time.as_millis(),
            diff_ms,
            window_ms = present_window.as_millis(),
            "A/V scheduler: waiting for target media time"
        );
        record_pipeline_pause(session, tick_result, PipelinePauseReason::SyncWaiting);
        repeat_present_video_frame(session, tick_result, None);
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    }

    tracing::debug!(
        pts_ms = frame.pts.as_millis(),
        audio_ms = audio_now.as_millis(),
        clock_ms = presentation_now.as_millis(),
        target_ms = target_media_time.as_millis(),
        diff_ms,
        window_ms = present_window.as_millis(),
        "A/V scheduler: frame selected"
    );
    present_front_queued_video_frame(session, tick_result);
    session.record_pipeline_latency(
        PipelineLatencyStage::WorkerScheduler,
        scheduler_started_at.elapsed(),
        None,
        None,
    );
}

/// Записывает drop одновременно в tick telemetry и session diagnostics.
fn record_video_drop(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    pts: Duration,
    reason: PlayerVideoDropReason,
) {
    session.record_video_drop(Some(pts), reason);
    tick_result.record_dropped_video_frame(pts, reason);
}

/// Записывает pipeline pause одновременно в tick telemetry и session diagnostics.
fn record_pipeline_pause(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    reason: PipelinePauseReason,
) {
    session.record_pipeline_pause(reason);
    tick_result.record_pipeline_pause(reason);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlayerCommand, PlayerSession};

    /// Создаёт test frame без реальных GPU resources с явным decoded contract.
    fn decoded_frame_with_format(
        pts: Duration,
        handle: u64,
        format: video_core::DecodedPixelFormat,
    ) -> video_core::DecodedFrame {
        let (bit_depth, memory_path) = match format {
            video_core::DecodedPixelFormat::Nv12 => (
                codec_core::BitDepth::Eight,
                video_core::FrameMemoryPath::DmaBufZeroCopy,
            ),
            video_core::DecodedPixelFormat::P010 => (
                codec_core::BitDepth::Ten,
                video_core::FrameMemoryPath::DmaBufZeroCopy,
            ),
        };

        video_core::DecodedFrame {
            pts,
            format,
            bit_depth,
            chroma: codec_core::ChromaSubsampling::Yuv420,
            memory_path,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: codec_core::VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: video_core::FrameTextureHandle(handle),
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    /// Создаёт текущий production NV12 test frame без привязки scheduler assertions к формату.
    fn decoded_frame(pts: Duration, handle: u64) -> video_core::DecodedFrame {
        decoded_frame_with_format(pts, handle, video_core::DecodedPixelFormat::Nv12)
    }

    #[test]
    fn scheduler_presents_first_ready_frame() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .pipeline
            .video_frame_queue
            .push_back(decoded_frame(Duration::ZERO, 1));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame
                .as_ref()
                .map(|frame| frame.pts),
            Some(Duration::ZERO)
        );
        assert!(session.pipeline.video_frame_queue.is_empty());
    }

    #[test]
    fn scheduler_waits_for_future_frame() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .pipeline
            .video_frame_queue
            .push_back(decoded_frame(Duration::from_secs(1), 1));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_presented, 0);
        assert!(session.pipeline.present_video_frame.is_none());
        assert_eq!(session.pipeline.video_frame_queue.len(), 1);
    }

    #[test]
    fn scheduler_uses_fallback_position_when_audio_clock_is_absent() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.update_current_position(Duration::from_millis(100));
        session
            .pipeline
            .video_frame_queue
            .push_back(decoded_frame(Duration::from_millis(100), 1));

        let tick_config = PlayerTickConfig {
            position_fallback_delta: Duration::ZERO,
            ..PlayerTickConfig::default()
        };
        let tick_result = session.tick(PlayerTickContext::with_config(Instant::now(), tick_config));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame
                .as_ref()
                .map(|frame| frame.pts),
            Some(Duration::from_millis(100))
        );
        assert!(session.pipeline.video_frame_queue.is_empty());
    }

    #[test]
    fn scheduler_late_drop_requires_next_frame_to_be_ready() {
        let mut queue = VecDeque::new();
        queue.push_back(decoded_frame(Duration::from_millis(0), 1));
        queue.push_back(decoded_frame(Duration::from_millis(33), 2));

        let should_drop = should_drop_front_frame_as_late(
            &queue,
            Duration::from_millis(70),
            Duration::from_millis(16),
        );

        assert!(should_drop);
    }

    #[test]
    fn scheduler_keeps_late_frame_without_replacement() {
        let mut queue = VecDeque::new();
        queue.push_back(decoded_frame(Duration::from_millis(0), 1));

        let should_drop = should_drop_front_frame_as_late(
            &queue,
            Duration::from_millis(70),
            Duration::from_millis(16),
        );

        assert!(!should_drop);
    }

    #[test]
    fn scheduler_repeats_current_frame_when_queue_is_empty() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.pipeline.present_video_frame = Some(decoded_frame(Duration::ZERO, 1));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_repeated, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame
                .as_ref()
                .map(|frame| frame.pts),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn scheduler_preserves_p010_boundary_frame_without_format_branching() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .pipeline
            .video_frame_queue
            .push_back(decoded_frame_with_format(
                Duration::ZERO,
                10,
                video_core::DecodedPixelFormat::P010,
            ));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame
                .as_ref()
                .map(|frame| frame.format),
            Some(video_core::DecodedPixelFormat::P010)
        );
        assert!(session.pipeline.video_frame_queue.is_empty());
    }

    #[test]
    fn decoder_thread_error_maps_to_runtime_player_error() {
        let decode_thread_error =
            video_vaapi::DecodeThreadError::new("P010 DMA-BUF zero-copy import failed");

        let player_error = player_error_from_decode_thread_error(&decode_thread_error);

        assert_eq!(player_error.kind, PlayerErrorKind::RuntimeError);
        assert!(
            player_error
                .message
                .contains("P010 DMA-BUF zero-copy import failed")
        );
    }

    #[test]
    fn audio_demux_catchup_requires_selected_audio_and_low_buffer() {
        assert!(audio_demux_catchup_needed_for_level(true, Some(40.0), 50.0));
        assert!(!audio_demux_catchup_needed_for_level(
            true,
            Some(60.0),
            50.0
        ));
        assert!(!audio_demux_catchup_needed_for_level(
            false,
            Some(40.0),
            50.0
        ));
        assert!(!audio_demux_catchup_needed_for_level(true, None, 50.0));
    }

    #[test]
    fn seek_video_preroll_is_capped_by_present_queue_capacity() {
        let tick_config = PlayerTickConfig {
            max_video_present_queue: 1,
            seek_resume_video_min_ready_frames: 8,
            ..PlayerTickConfig::default()
        };

        assert_eq!(
            effective_seek_resume_video_min_ready_frames(&tick_config),
            2
        );
    }

    #[test]
    fn audio_demux_catchup_uses_bounded_video_packet_limit() {
        let mut session = PlayerSession::new();
        let tick_config = PlayerTickConfig {
            max_pending_video_packets: 2,
            max_pending_video_packets_during_audio_catchup: 4,
            ..PlayerTickConfig::default()
        };

        for packet_index in 0..tick_config.max_pending_video_packets {
            session
                .pipeline
                .pending_video_packets
                .push_back(PendingVideoPacket::new(
                    TrackId::new(1),
                    Duration::from_millis(packet_index as u64),
                    session.pipeline.seek_generation,
                    Bytes::new(),
                    false,
                ));
        }

        assert!(!can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            false
        ));
        assert!(can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            true
        ));

        for packet_index in tick_config.max_pending_video_packets
            ..tick_config.max_pending_video_packets_during_audio_catchup
        {
            session
                .pipeline
                .pending_video_packets
                .push_back(PendingVideoPacket::new(
                    TrackId::new(1),
                    Duration::from_millis(packet_index as u64),
                    session.pipeline.seek_generation,
                    Bytes::new(),
                    false,
                ));
        }

        assert!(!can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            true
        ));
    }

    #[test]
    fn route_demuxed_audio_packet_preserves_shared_payload_and_metadata() {
        let mut session = PlayerSession::new();
        let payload = Bytes::from(vec![0x4f, 0x70, 0x75, 0x73]);
        let payload_ptr = payload.as_ptr();
        let packet = media_core::Packet::new(
            TrackId::new(2),
            TrackKind::Audio,
            Duration::from_millis(42),
            None,
            false,
            payload.clone(),
        );

        route_demuxed_packet(&mut session, packet);

        let pending_packet = session
            .pipeline
            .pending_audio_packets
            .front()
            .expect("audio packet должен попасть в pending audio queue");

        assert_eq!(pending_packet.track_id, TrackId::new(2));
        assert_eq!(pending_packet.pts, Duration::from_millis(42));
        assert_eq!(pending_packet.generation, session.pipeline.seek_generation);
        assert_eq!(pending_packet.encoded_bytes.as_ptr(), payload_ptr);
        assert_eq!(&pending_packet.encoded_bytes[..], b"Opus");
    }

    #[test]
    fn route_demuxed_video_packet_preserves_shared_payload_keyframe_and_pts() {
        let mut session = PlayerSession::new();
        let payload = Bytes::from(vec![0x82, 0x49, 0x83, 0x42]);
        let payload_ptr = payload.as_ptr();
        let packet = media_core::Packet::new(
            TrackId::new(1),
            TrackKind::Video,
            Duration::from_millis(120),
            None,
            true,
            payload.clone(),
        );

        route_demuxed_packet(&mut session, packet);

        let pending_packet = session
            .pipeline
            .pending_video_packets
            .front()
            .expect("video packet должен попасть в pending video queue");

        assert_eq!(pending_packet.track_id, TrackId::new(1));
        assert_eq!(pending_packet.pts, Duration::from_millis(120));
        assert_eq!(pending_packet.generation, session.pipeline.seek_generation);
        assert!(pending_packet.keyframe);
        assert_eq!(pending_packet.encoded_bytes.as_ptr(), payload_ptr);
        assert_eq!(&pending_packet.encoded_bytes[..], &[0x82, 0x49, 0x83, 0x42]);
    }

    #[test]
    fn decoder_bootstrap_drops_interframes_until_keyframe() {
        let mut session = PlayerSession::new();
        session.pipeline.video_decoder_needs_keyframe = true;

        assert!(!accept_video_packet_for_decoder_bootstrap(
            &mut session,
            false,
            Duration::from_millis(10)
        ));
        assert!(session.pipeline.video_decoder_needs_keyframe);

        assert!(accept_video_packet_for_decoder_bootstrap(
            &mut session,
            true,
            Duration::from_millis(20)
        ));
        assert!(!session.pipeline.video_decoder_needs_keyframe);

        assert!(accept_video_packet_for_decoder_bootstrap(
            &mut session,
            false,
            Duration::from_millis(30)
        ));
    }

    #[test]
    fn vp9_12bit_packet_returns_exact_player_diagnostic() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 12,
            subsampling_x: true,
            subsampling_y: true,
            width: 128,
            height: 72,
        });

        let error = vp9_requirement_from_packet(&packet_bytes, None)
            .expect_err("VP9 Profile 2 12-bit должен rejected до hardware matching");

        assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoBitDepth);
        assert!(error.message.contains("12-bit"));
    }

    #[test]
    fn vp9_unsupported_chroma_packet_returns_exact_player_diagnostic() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 1,
            bit_depth: 8,
            subsampling_x: true,
            subsampling_y: false,
            width: 128,
            height: 72,
        });

        let error = vp9_requirement_from_packet(&packet_bytes, None)
            .expect_err("VP9 Profile 1 4:2:2 должен rejected до hardware matching");

        assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoChroma);
        assert!(error.message.contains("4:2:2"));
    }

    #[test]
    fn vp9_incomplete_packet_stays_recoverable_for_decoder() {
        let requirement = vp9_requirement_from_packet(&[0x00], None)
            .expect("неполный VP9 header не должен становиться fatal reject");

        assert!(requirement.is_none());
    }

    struct Vp9HeaderFixture {
        profile: u8,
        bit_depth: u8,
        subsampling_x: bool,
        subsampling_y: bool,
        width: u32,
        height: u32,
    }

    fn build_vp9_keyframe(fixture: Vp9HeaderFixture) -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, 0b10, 2);
        push_profile(&mut bits, fixture.profile);
        bits.push(0);
        bits.push(0);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, 0x498342, 24);
        if matches!(fixture.profile, 2 | 3) {
            bits.push(u8::from(fixture.bit_depth == 12));
        }
        push_bits(&mut bits, 1, 3);
        bits.push(0);
        if matches!(fixture.profile, 1 | 3) {
            bits.push(u8::from(fixture.subsampling_x));
            bits.push(u8::from(fixture.subsampling_y));
            bits.push(0);
        }
        push_bits(&mut bits, fixture.width - 1, 16);
        push_bits(&mut bits, fixture.height - 1, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                let mut byte = 0u8;
                for (index, bit) in chunk.iter().enumerate() {
                    byte |= bit << (7 - index);
                }
                byte
            })
            .collect()
    }

    fn push_bits(bits: &mut Vec<u8>, value: u32, width: u8) {
        for shift in (0..width).rev() {
            bits.push(((value >> shift) & 1) as u8);
        }
    }

    fn push_profile(bits: &mut Vec<u8>, profile: u8) {
        bits.push(profile & 1);
        bits.push((profile >> 1) & 1);
        if profile == 3 {
            bits.push(0);
        }
    }
}
