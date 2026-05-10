//! Playback tick и A/V scheduler.
//!
//! Этот модуль держит логику, которая раньше жила в `app-egui::main`:
//! чтение packets из demuxer, audio throttle, отправку video packets в decoder,
//! приём decoded frames, backpressure и выбор кадра для показа.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use codec_core::{
    VideoCodec, VideoDecodeRequirement, Vp9RequirementProbe, Vp9RequirementRejection,
    probe_vp9_packet_requirement,
};
use media_core::{TrackId, TrackKind};
use rustiplayer_config::AppConfig;
use tracing::{trace, warn};

use crate::{
    PendingAudioPacket, PendingVideoPacket, PlaybackState, PlayerError, PlayerErrorKind,
    PlayerSession,
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

    /// Максимум video packets, отправляемых decoder thread за один tick.
    pub max_video_packets_sent_per_tick: usize,

    /// Максимум decoded frames, принимаемых из decoder thread за один tick.
    pub max_decoded_video_frames_drained_per_tick: usize,

    /// Максимальный decode-ahead относительно audio clock.
    pub max_video_decode_ahead: Duration,

    /// Уровень audio buffer, выше которого audio packets временно не декодируются.
    pub audio_buffer_high_water_mark_ms: f64,

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
            max_video_packets_sent_per_tick: 1,
            max_decoded_video_frames_drained_per_tick: 1,
            max_video_decode_ahead: Duration::from_millis(500),
            audio_buffer_high_water_mark_ms: 200.0,
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
            ..defaults
        }
    }
}

/// Итог работы одного playback tick для shell-телеметрии.
#[derive(Debug, Clone, Default)]
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

    /// Признак keyframe для video packets.
    pub keyframe: bool,
}

/// Причина удаления video frame внутри scheduler-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerVideoDropReason {
    /// Кадр устарел относительно audio-master media time.
    Late,

    /// Кадр вытеснен из-за переполнения presentation queue.
    QueueOverflow,

    /// Кадр пришёл после пользовательской паузы.
    Paused,
}

/// Summary удалённого video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerVideoFrameDrop {
    /// Presentation timestamp удалённого кадра.
    pub pts: Duration,

    /// Причина удаления кадра.
    pub reason: PlayerVideoDropReason,
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

            self.process_audio_packet(packet.track_id, &packet.data);
        }
    }

    /// Обновляет playback position один раз за tick.
    fn update_position_for_tick(&mut self, position_fallback_delta: Duration) {
        if self.playback_state() != PlaybackState::Playing {
            return;
        }

        if let Some(audio_secs) = self.audio_clock_secs() {
            if let Ok(audio_position) = Duration::try_from_secs_f64(audio_secs) {
                self.update_current_position(audio_position);
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

/// Читает новые packets из demuxer в пределах бюджета текущего tick.
fn read_demux_packets(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
) {
    for _ in 0..tick_config.max_demux_packets_per_tick {
        if !can_read_next_demux_packet(session, tick_config) {
            tick_result.demux_backpressured = true;
            trace!(
                pending_video_packets = session.pipeline.pending_video_packets.len(),
                queued_video_frames = session.pipeline.video_frame_queue.len(),
                "Demux backpressure: waiting for decoder/presentation"
            );
            break;
        }

        let packet_result = {
            let Some(demuxer) = session.pipeline.demuxer.as_mut() else {
                break;
            };
            demuxer.next_packet()
        };

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
    match packet.kind {
        TrackKind::Audio => {
            session
                .pipeline
                .pending_audio_packets
                .push_back(PendingAudioPacket::new(
                    packet.track_id,
                    packet.data.to_vec(),
                ));
        }
        TrackKind::Video => {
            session
                .pipeline
                .pending_video_packets
                .push_back(PendingVideoPacket::new(
                    packet.track_id,
                    packet.pts,
                    packet.data.to_vec(),
                    packet.keyframe,
                ));
        }
    }
}

/// Проверяет, можно ли читать следующий packet из demuxer.
fn can_read_next_demux_packet(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
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
    let packet_lead = packet_pts.saturating_sub(audio_now);

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
        tick_result
            .record_dropped_video_frame(stale_frame.pts, PlayerVideoDropReason::QueueOverflow);
        tracing::debug!(
            pts_ms = stale_frame.pts.as_millis(),
            queue_limit,
            "Dropping oldest queued video frame before enqueue"
        );
    }

    session.pipeline.video_frame_queue.push_back(frame);
}

/// Забирает готовые кадры из decoder thread и кладёт их в presentation queue.
fn drain_decoded_video_frames(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
) {
    let Some(ref thread) = session.pipeline.video_decoder_thread else {
        if !session.pipeline.pending_video_packets.is_empty() {
            tracing::warn!(
                count = session.pipeline.pending_video_packets.len(),
                "No video decoder thread — dropping video packets"
            );
        }
        session.pipeline.pending_video_packets.clear();
        return;
    };

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

    let mut decoded_frames = Vec::new();
    for _ in 0..receive_budget {
        let Some(frame) = thread.try_recv_frame() else {
            break;
        };
        decoded_frames.push(frame);
    }

    for frame in decoded_frames {
        tracing::debug!(
            pts_ms = frame.pts.as_millis(),
            width = frame.width,
            height = frame.height,
            "Video frame decoded"
        );
        tick_result.record_decoded_video_frame();
        session.observe_video_frame_pts(frame.pts);

        if playback_can_present {
            enqueue_decoded_video_frame(session, tick_config, tick_result, frame);
        } else {
            release_video_texture(session, frame.texture_handle);
            tick_result.record_dropped_video_frame(frame.pts, PlayerVideoDropReason::Paused);
            tracing::debug!("Dropping decoded frame received while playback is paused");
        }
    }
}

/// Отправляет ограниченное число pending video packets в decoder thread.
fn send_pending_video_packets_to_decoder(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
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
        let packet_data = packet.data.clone();

        if session.pipeline.video_track_id != Some(packet_track_id) {
            session.pipeline.pending_video_packets.pop_front();
            continue;
        }

        let packet_probe = PendingVideoPacketProbe {
            track_id: packet_track_id,
            data: packet_data,
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
            data_len = packet.data.len(),
            keyframe = packet.keyframe,
            "Sending video packet to decoder thread"
        );

        let decode_packet = video_vaapi::DecodePacket {
            track_id: packet.track_id,
            pts: packet.pts,
            data: packet.data,
            keyframe: packet.keyframe,
        };

        let Some(ref thread) = session.pipeline.video_decoder_thread else {
            break;
        };
        if let Err(error) = thread.send_packet(decode_packet) {
            tracing::warn!(error = %error, "Failed to send packet to decoder thread");
            break;
        }

        sent_packets += 1;
    }
}

/// Минимальный view pending packet-а для bitstream capability validation.
struct PendingVideoPacketProbe {
    /// Track ID нужен, чтобы найти container codec.
    track_id: TrackId,

    /// Codec payload нужен VP9 parser-у для чтения profile из uncompressed header.
    data: Vec<u8>,
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
        .and_then(|requirement| requirement.profile)
        .is_some()
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
) -> Result<Option<VideoDecodeRequirement>, PlayerError> {
    match probe_vp9_packet_requirement(packet_data) {
        Vp9RequirementProbe::Candidate(candidate) => Ok(Some(candidate.requirement)),
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
        VideoCodec::Vp9 => vp9_requirement_from_packet(&packet.data),
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
        tick_result.record_dropped_video_frame(frame.pts, PlayerVideoDropReason::QueueOverflow);
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
    audio_now: Duration,
) -> Duration {
    let lead_frames = finite_non_negative_factor(
        tick_config.video_present_lead_frames,
        PlayerTickConfig::default().video_present_lead_frames,
    );
    let present_lead = session
        .pipeline
        .video_frame_duration_estimate
        .mul_f64(lead_frames);

    saturating_duration_add(audio_now, present_lead)
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
fn release_video_texture(session: &PlayerSession, texture_handle: video_core::FrameTextureHandle) {
    if let Some(ref thread) = session.pipeline.video_decoder_thread {
        thread.release_frame(texture_handle);
    }
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
    tick_result.record_dropped_video_frame(frame_pts, reason);
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
    session.pipeline.present_video_frame = Some(frame);
    tick_result.record_presented_video_frame();
    true
}

/// Повторно показывает текущий кадр и учитывает это в telemetry result.
fn repeat_present_video_frame(session: &PlayerSession, tick_result: &mut PlayerTickResult) {
    if session.pipeline.present_video_frame.is_some() {
        tick_result.record_repeated_video_frame();
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
        send_pending_video_packets_to_decoder(session, tick_config);
    }

    trim_video_present_queue(session, tick_config, tick_result);

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
            repeat_present_video_frame(session, tick_result);
        }
        return;
    }

    let target_media_time = target_media_time_for_present(session, tick_config, audio_now);
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
        repeat_present_video_frame(session, tick_result);
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
        repeat_present_video_frame(session, tick_result);
        return;
    }

    tracing::debug!(
        pts_ms = frame.pts.as_millis(),
        audio_ms = audio_now.as_millis(),
        target_ms = target_media_time.as_millis(),
        diff_ms,
        window_ms = present_window.as_millis(),
        "A/V scheduler: frame selected"
    );
    present_front_queued_video_frame(session, tick_result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlayerCommand, PlayerSession};

    /// Создаёт test frame без реальных GPU resources.
    fn decoded_frame(pts: Duration, handle: u64) -> video_core::DecodedFrame {
        video_core::DecodedFrame {
            pts,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: codec_core::VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: video_core::FrameTextureHandle(handle),
        }
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
    fn vp9_12bit_packet_returns_exact_player_diagnostic() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 12,
            subsampling_x: true,
            subsampling_y: true,
            width: 128,
            height: 72,
        });

        let error = vp9_requirement_from_packet(&packet_bytes)
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

        let error = vp9_requirement_from_packet(&packet_bytes)
            .expect_err("VP9 Profile 1 4:2:2 должен rejected до hardware matching");

        assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoChroma);
        assert!(error.message.contains("4:2:2"));
    }

    #[test]
    fn vp9_incomplete_packet_stays_recoverable_for_decoder() {
        let requirement = vp9_requirement_from_packet(&[0x00])
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
