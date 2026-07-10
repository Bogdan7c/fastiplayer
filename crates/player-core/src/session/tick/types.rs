use std::time::{Duration, Instant};

use media_core::{PacketKeyframe, TrackId, TrackKind, TrackTimestamp};
use rustiplayer_config::AppConfig;

use super::presentation_scheduler::video_present_queue_limit;
use crate::PipelinePauseReason;

/// Контекст одного playback tick.
#[derive(Debug, Clone, Copy)]
pub struct PlayerTickContext {
    /// Монотонное время shell на момент tick.
    pub now: Instant,

    /// Настройки scheduler/backpressure для текущего tick.
    pub config: PlayerTickConfig,

    /// Насколько worker опоздал относительно своего регулярного tick interval.
    pub tick_late_by: Duration,
}

impl PlayerTickContext {
    /// Создаёт tick context с production defaults.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            now,
            config: PlayerTickConfig::default(),
            tick_late_by: Duration::ZERO,
        }
    }

    /// Создаёт tick context с явно переданным конфигом.
    #[must_use]
    pub const fn with_config(now: Instant, config: PlayerTickConfig) -> Self {
        Self {
            now,
            config,
            tick_late_by: Duration::ZERO,
        }
    }

    /// Создаёт tick context с worker timing diagnostics для adaptive catch-up.
    #[must_use]
    pub const fn with_timing(
        now: Instant,
        config: PlayerTickConfig,
        tick_late_by: Duration,
    ) -> Self {
        Self {
            now,
            config,
            tick_late_by,
        }
    }
}

/// Конфигурация playback tick, backpressure и A/V scheduler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerTickConfig {
    /// Сколько container packets можно прочитать за один tick.
    pub max_demux_packets_per_tick: usize,

    /// Максимум decoded video frames в очереди presentation.
    pub max_video_present_queue: usize,

    /// Минимальный запас decoded frames, ниже которого считаем pipeline starvation-prone.
    pub min_video_present_queue: usize,

    /// Целевой запас decoded frames в очереди presentation.
    pub target_video_present_queue: usize,

    /// Минимальный запас свободных texture slots перед отправкой новых packets в decoder.
    pub min_texture_slots_available_for_decode: usize,

    /// Целевой запас свободных texture/surface slots для adaptive catch-up.
    pub target_texture_slots_available_for_decode: usize,

    /// Максимум сырых video packets между demuxer и decoder thread.
    pub max_pending_video_packets: usize,

    /// Максимум готовых decoded frames внутри decoder ready queue.
    pub decoder_ready_queue_frames: usize,

    /// Временный bounded лимит video packets, пока audio buffer догоняет low-watermark.
    pub max_pending_video_packets_during_audio_catchup: usize,

    /// Максимум compressed video packets в recovery staging до safe rollback-а.
    ///
    /// Это отдельный лимит от decoder-facing backlog: длинный GOP может быть
    /// больше обычной catch-up очереди, но staging всё равно обязан быть bounded.
    pub max_video_backlog_recovery_scan_packets: usize,

    /// Максимум retained compressed payload recovery staging-а в bytes.
    pub max_video_backlog_recovery_scan_bytes: usize,

    /// Максимум video packets, отправляемых decoder thread за один tick.
    pub max_video_packets_sent_per_tick: usize,

    /// Максимум decoded frames, принимаемых из decoder thread за один tick.
    pub max_decoded_video_frames_drained_per_tick: usize,

    /// Максимальный decode-ahead относительно audio clock.
    pub max_video_decode_ahead: Duration,

    /// Целевой steady-state decode-ahead относительно audio clock.
    pub target_video_decode_ahead: Duration,

    /// Дополнительное bounded окно catch-up work после базового tick.
    pub adaptive_catch_up_time_budget: Duration,

    /// Bounded окно fast-preroll work для active accurate seek.
    pub seek_fast_preroll_time_budget: Duration,

    /// Burst-лимит video packets/frames для active accurate seek preroll.
    pub seek_fast_preroll_video_packet_burst: usize,

    /// Уровень audio buffer, выше которого audio packets временно не декодируются.
    pub audio_buffer_high_water_mark_ms: f64,

    /// Уровень audio buffer, ниже которого demux может читать сквозь video backpressure.
    pub audio_demux_low_water_mark_ms: f64,

    /// Минимальный audio buffer перед переходом autoplay из `Buffering` в `Playing`.
    pub audio_preroll_target_ms: f64,

    /// Максимальное время ожидания seek commit gates.
    pub seek_commit_timeout: Duration,

    /// Минимальный audio buffer перед resume после seek.
    pub seek_resume_audio_min_buffer_ms: f64,

    /// Soft timeout audio gate-а после того, как target video frame уже показан.
    pub seek_resume_audio_gate_timeout: Duration,

    /// Минимальный запас готовых video frames перед resume после seek.
    pub seek_resume_video_min_ready_frames: usize,

    /// Минимальная позиция audio clock, после которой stalled audio считается реальным.
    pub audio_stall_min_position: Duration,

    /// Длительность без движения audio clock, после которой звук считается stalled.
    pub audio_stall_timeout: Duration,

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
            max_demux_packets_per_tick: 12,
            max_video_present_queue: 8,
            min_video_present_queue: 2,
            target_video_present_queue: 4,
            min_texture_slots_available_for_decode: 2,
            target_texture_slots_available_for_decode: 4,
            max_pending_video_packets: 32,
            decoder_ready_queue_frames: 8,
            max_pending_video_packets_during_audio_catchup: 240,
            // Target HDR AV1 asset имеет до 420 frames между keyframes; 512
            // оставляет bounded запас без добавления playback-rate TOML knob-а.
            max_video_backlog_recovery_scan_packets: 512,
            // Измеренный максимум GOP payload для target asset равен 20.63 MiB.
            max_video_backlog_recovery_scan_bytes: 32 * 1024 * 1024,
            max_video_packets_sent_per_tick: 8,
            max_decoded_video_frames_drained_per_tick: 8,
            max_video_decode_ahead: Duration::from_millis(500),
            target_video_decode_ahead: Duration::from_millis(250),
            adaptive_catch_up_time_budget: Duration::from_millis(4),
            seek_fast_preroll_time_budget: Duration::from_millis(48),
            seek_fast_preroll_video_packet_burst: 512,
            audio_buffer_high_water_mark_ms: 200.0,
            audio_demux_low_water_mark_ms: 100.0,
            audio_preroll_target_ms: 50.0,
            seek_commit_timeout: Duration::from_millis(10_000),
            seek_resume_audio_min_buffer_ms: 50.0,
            seek_resume_audio_gate_timeout: Duration::from_millis(250),
            seek_resume_video_min_ready_frames: 3,
            audio_stall_min_position: Duration::from_millis(100),
            audio_stall_timeout: Duration::from_millis(250),
            video_present_lead_frames: 0.5,
            video_present_window_frames: 1.0,
            video_late_drop_grace_frames: 2.0,
        }
    }
}

impl From<&AppConfig> for PlayerTickConfig {
    /// Собирает runtime-лимиты playback из пользовательского TOML-config.
    ///
    /// Scheduler knobs читаются из `[video.scheduler]`; старые top-level video
    /// поля остаются max-границами для present queue, decode-ahead и bounded
    /// decoder queues.
    fn from(config: &AppConfig) -> Self {
        let defaults = Self::default();

        Self {
            max_demux_packets_per_tick: config.video.scheduler.demux_packets_per_tick,
            max_video_present_queue: config.video.present_queue_frames,
            min_video_present_queue: config.video.scheduler.present_queue_min_frames,
            target_video_present_queue: config.video.scheduler.present_queue_target_frames,
            min_texture_slots_available_for_decode: config.video.scheduler.surface_free_slots_min,
            target_texture_slots_available_for_decode: config
                .video
                .scheduler
                .surface_free_slots_target,
            max_pending_video_packets: config.video.decoder_packet_channel_frames,
            decoder_ready_queue_frames: config.video.decoder_ready_queue_frames,
            max_video_packets_sent_per_tick: config.video.scheduler.video_packets_per_tick,
            max_decoded_video_frames_drained_per_tick: config
                .video
                .scheduler
                .decoded_frames_per_tick,
            max_video_decode_ahead: Duration::from_millis(config.video.max_decode_ahead_ms),
            target_video_decode_ahead: Duration::from_millis(
                config.video.scheduler.decode_ahead_target_ms,
            ),
            adaptive_catch_up_time_budget: Duration::from_millis(
                config.video.scheduler.catch_up_budget_ms,
            ),
            seek_fast_preroll_time_budget: Duration::from_millis(
                config.player.seek.fast_preroll_budget_ms,
            ),
            seek_fast_preroll_video_packet_burst: config
                .player
                .seek
                .fast_preroll_video_packet_burst,
            audio_buffer_high_water_mark_ms: config.audio.buffer_target_ms as f64,
            audio_demux_low_water_mark_ms: (config.audio.buffer_target_ms as f64 * 0.5)
                .max(config.player.seek.resume_audio_min_buffer_ms as f64),
            audio_preroll_target_ms: config.player.seek.resume_audio_min_buffer_ms as f64,
            seek_commit_timeout: Duration::from_millis(config.player.seek.commit_timeout_ms),
            seek_resume_audio_min_buffer_ms: config.player.seek.resume_audio_min_buffer_ms as f64,
            seek_resume_audio_gate_timeout: Duration::from_millis(
                config.player.seek.resume_audio_gate_timeout_ms,
            ),
            seek_resume_video_min_ready_frames: config.player.seek.resume_video_min_ready_frames,
            ..defaults
        }
    }
}

impl PlayerTickConfig {
    /// Возвращает достижимый video preroll для seek resume с учётом размера presentation queue.
    #[must_use]
    pub(crate) fn effective_seek_resume_video_min_ready_frames(&self) -> usize {
        self.seek_resume_video_min_ready_frames
            .max(1)
            .min(video_present_queue_limit(self).saturating_add(1))
    }
}

/// Итог работы одного playback tick для shell-телеметрии.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerTickResult {
    /// Packets, прочитанные из demuxer за tick.
    pub demuxed_packets: Vec<PlayerTickPacket>,

    /// Audio packets, полностью отброшенные как accurate-seek preroll и не
    /// записанные в `demuxed_packets`, чтобы dense PCM seek не создавал
    /// unbounded per-packet telemetry allocations.
    pub dropped_seek_audio_preroll_packets: u64,

    /// Video packets, удержанные recovery staging-ом и агрегированные без
    /// per-packet `demuxed_packets` allocations.
    pub staged_video_backlog_recovery_packets: u64,

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

impl PlayerTickPacket {
    /// Отделяет bounded telemetry от codec payload до передачи packet-а pipeline-у.
    pub(super) fn from_demuxed_packet(packet: &media_core::Packet) -> Self {
        Self {
            track_id: packet.track_id,
            kind: packet.kind,
            pts: packet.pts,
            track_pts: packet.track_pts,
            track_dts: packet.track_dts,
            size: packet.data.len(),
            byte_offset: packet.byte_offset,
            keyframe: packet.keyframe,
        }
    }
}

impl PlayerTickResult {
    /// Запоминает уже отделённую от codec payload packet telemetry.
    pub(super) fn record_demuxed_packet(&mut self, packet: PlayerTickPacket) {
        self.demuxed_packets.push(packet);
    }

    /// Учитывает один staged recovery packet bounded scalar-ом.
    pub(super) fn record_staged_video_backlog_recovery_packet(&mut self) {
        self.staged_video_backlog_recovery_packets =
            self.staged_video_backlog_recovery_packets.saturating_add(1);
    }

    /// Учитывает dropped audio preroll без создания `PlayerTickPacket`.
    pub(super) fn record_dropped_seek_audio_preroll_packet(&mut self) {
        self.dropped_seek_audio_preroll_packets =
            self.dropped_seek_audio_preroll_packets.saturating_add(1);
    }

    /// Учитывает принятый decoded video frame.
    pub(super) fn record_decoded_video_frame(&mut self) {
        self.decoded_video_frames = self.decoded_video_frames.saturating_add(1);
    }

    /// Учитывает кадр, выбранный для presentation.
    pub(super) fn record_presented_video_frame(&mut self) {
        self.video_frames_presented = self.video_frames_presented.saturating_add(1);
    }

    /// Учитывает повтор текущего present frame.
    pub(super) fn record_repeated_video_frame(&mut self) {
        self.video_frames_repeated = self.video_frames_repeated.saturating_add(1);
    }

    /// Учитывает удалённый video frame вместе с причиной.
    pub(super) fn record_dropped_video_frame(
        &mut self,
        pts: Duration,
        reason: PlayerVideoDropReason,
    ) {
        self.dropped_video_frames
            .push(PlayerVideoFrameDrop { pts, reason });
    }

    /// Учитывает typed pipeline pause.
    pub(super) fn record_pipeline_pause(&mut self, reason: PipelinePauseReason) {
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

    /// Исходный signed PTS demuxer-а в track time base, если доступен.
    pub track_pts: Option<TrackTimestamp>,

    /// Исходный signed DTS demuxer-а в track time base, если доступен.
    pub track_dts: Option<TrackTimestamp>,

    /// Размер codec payload в bytes.
    pub size: usize,

    /// Safe source byte offset для demux seek, если container adapter его сообщил.
    pub byte_offset: Option<u64>,

    /// Keyframe-классификация для video packets.
    pub keyframe: PacketKeyframe,
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
