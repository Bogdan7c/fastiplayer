use std::collections::VecDeque;
use std::time::Duration;

use frame_server_core::{
    ScrubDiagnosticsRecorder, ScrubDiagnosticsSnapshot, ScrubEventDiagnostics,
};
use media_core::{PacketKeyframe, TrackId};
use video_core::{DecodedFrame, FrameMemoryPath, VideoFramePublishPressureDiagnostics};

/// Максимум latency samples, которые diagnostics держит в памяти.
const RECENT_WORST_SAMPLE_LIMIT: usize = 16;

/// Codec-neutral стадия playback pipeline для latency attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineLatencyStage {
    /// Чтение packet-а из source/demuxer.
    DemuxRead,

    /// Ожидание packet-а в bounded decoder packet channel.
    DecoderPacketReceive,

    /// Submit encoded packet-а в decoder backend.
    DecoderSubmit,

    /// Drain decoder events и ready queue.
    DecoderEventDrain,

    /// Ожидание готовности hardware decoded surface.
    HardwareSync,

    /// Экспорт decoded surface в external memory descriptor.
    DmaBufExport,

    /// Импорт external memory в renderer-visible GPU resource.
    DmaBufImport,

    /// Публикация decoded frame через bounded decoder->worker channel.
    DecodedFramePublish,

    /// Решение worker scheduler-а по queued/present frame.
    WorkerScheduler,

    /// Ожидание render thread-а при запросе present frame.
    RenderAcquire,

    /// Ожидание backend resource pool lock-а внутри renderer materialization boundary.
    RenderResourceLockWait,

    /// Submit/present render work, если renderer сообщил timing.
    GpuSubmitPresent,

    /// Задержка release ack от render side до worker.
    ReleaseAcknowledgement,
}

impl PipelineLatencyStage {
    /// Возвращает стабильное codec-neutral имя stage для logs/UI.
    #[must_use]
    pub const fn metric_name(self) -> &'static str {
        match self {
            Self::DemuxRead => "demux.read",
            Self::DecoderPacketReceive => "decoder.packet_receive",
            Self::DecoderSubmit => "decoder.submit",
            Self::DecoderEventDrain => "decoder.event_drain",
            Self::HardwareSync => "decoder.hardware_sync",
            Self::DmaBufExport => "zero_copy.export",
            Self::DmaBufImport => "zero_copy.import",
            Self::DecodedFramePublish => "decoder.frame_publish",
            Self::WorkerScheduler => "worker.scheduler",
            Self::RenderAcquire => "render.acquire",
            Self::RenderResourceLockWait => "render.resource_lock_wait",
            Self::GpuSubmitPresent => "gpu.submit_present",
            Self::ReleaseAcknowledgement => "release.ack",
        }
    }
}

/// Typed причина удаления video frame/packet-а в playback pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoDropReason {
    /// Кадр устарел относительно media clock.
    Late,

    /// Очередь вытеснила кадр из-за bounded capacity.
    QueueOverflow,

    /// Packet/frame принадлежал старому seek/render generation.
    StaleGeneration,

    /// Packet/frame отброшен как seek pre-roll до target кадра.
    SeekPreroll,

    /// Legacy: старый synchronous render request не получил reply в bounded budget.
    RenderAcquisitionTimeout,

    /// Pipeline не получил decoded frame, когда renderer ожидал новый кадр.
    DecoderStarvation,

    /// Кадр пришёл после пользовательской pause и не должен менять картинку.
    Paused,
}

impl VideoDropReason {
    /// Возвращает стабильное имя причины для logs/UI.
    #[must_use]
    pub const fn metric_name(self) -> &'static str {
        match self {
            Self::Late => "late",
            Self::QueueOverflow => "queue_overflow",
            Self::StaleGeneration => "stale_generation",
            Self::SeekPreroll => "seek_preroll",
            Self::RenderAcquisitionTimeout => "render_acquisition_timeout",
            Self::DecoderStarvation => "decoder_starvation",
            Self::Paused => "paused",
        }
    }
}

/// Typed причина временной паузы pipeline без удаления конкретного frame-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelinePauseReason {
    /// Demux не читает дальше из-за downstream backpressure.
    DemuxBackpressure,

    /// Decode остановлен до освобождения surface/import slot-а.
    WaitingForFreeSurface,

    /// Decode/demux ждёт свободное место в presentation queue.
    WaitingForPresentQueue,

    /// Surface ждёт renderer GPU completion/release path.
    WaitingForGpuRelease,

    /// Video временно уступает demux/audio catch-up policy.
    WaitingForDemuxAudioPriority,

    /// Bounded packet channel decoder thread-а заполнен.
    DecoderPacketQueueFull,

    /// Bounded control/release channel decoder thread-а заполнен.
    DecoderControlQueueFull,

    /// Software host-upload ready frame queue заполнена.
    HostUploadReadyQueueFull,

    /// Software host-upload slots заняты и ждут release.
    HostUploadSlotsExhausted,

    /// Decoder/presentation queue не дала кадр к текущему render request.
    DecoderStarvation,

    /// Scheduler ждёт media time для слишком раннего кадра.
    SyncWaiting,

    /// Legacy: synchronous render request не получил reply в bounded budget.
    RenderAcquireTimeout,
}

/// Snapshot texture/surface pressure без backend-specific handles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextureSlotPressureSnapshot {
    /// Максимальное количество slots.
    pub capacity: usize,

    /// Количество созданных slots.
    pub slots: usize,

    /// Количество slots, удерживаемых live кадрами.
    pub in_use: usize,

    /// Количество persistent imports, доступных для reuse.
    pub free_surfaces: usize,

    /// Количество releases, ожидающих GPU completion.
    pub waiting_gpu_completion: usize,

    /// Количество releases, ожидающих decoder reuse ack.
    pub waiting_decoder_reuse: usize,

    /// Количество failed external imports.
    pub import_failures: u64,

    /// Количество созданных external imports.
    pub imports_created: u64,

    /// Количество reuse hits без нового external import-а.
    pub imports_reused: u64,

    /// Количество replacements free import-а после смены descriptor/object identity.
    pub imports_replaced: u64,
}

impl TextureSlotPressureSnapshot {
    /// Возвращает свободные slots без underflow.
    #[must_use]
    pub const fn available_slots(self) -> usize {
        self.capacity.saturating_sub(self.in_use)
    }
}

/// Backwards-compatible public name для neutral decoder control-channel diagnostics.
pub use video_core::VideoDecoderControlChannelPressureSnapshot as DecoderControlChannelPressureSnapshot;

/// Queue depths, снятые около latency/drop события.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineQueueDepthSnapshot {
    /// Audio packets перед decoder/audio output.
    pub pending_audio_packets: usize,

    /// Video packets перед decoder thread.
    pub pending_video_packets: usize,

    /// Compressed video packets, временно удерживаемые bounded recovery scan-ом.
    pub staged_video_backlog_recovery_packets: usize,

    /// Retained compressed payload bounded recovery scan-а.
    pub staged_video_backlog_recovery_bytes: usize,

    /// Player presentation queue depth.
    pub present_queue_depth: usize,

    /// Decoder send queue depth около отправки packet-а.
    pub decoder_send_queue_depth: usize,

    /// Packets, которые decoder thread уже забрал из channel, но ещё не вернул frame.
    pub decoder_in_flight_packets: usize,

    /// Backend ready queue depth, если decoder сообщил его с frame-ом.
    pub decoder_ready_queue_depth: Option<usize>,

    /// Активные render leases.
    pub active_render_leases: usize,

    /// Texture releases, ожидающие drop render lease-а.
    pub deferred_render_releases: usize,

    /// Texture/surface slot pressure.
    pub texture_slots: Option<TextureSlotPressureSnapshot>,

    /// Pressure/failure counters bounded decoder control channel-а.
    pub decoder_control_channel: Option<DecoderControlChannelPressureSnapshot>,
}

/// Worst/average latency по одной stage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LatencyCounterSnapshot {
    /// Количество samples.
    pub samples: u64,

    /// Средняя latency для быстрых сравнений в diagnostics UI.
    pub average: Duration,

    /// Худший sample этой stage.
    pub worst: Option<PipelineLatencySampleSnapshot>,
}

/// Один latency sample, сохранённый без heap-строк и backend handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineLatencySampleSnapshot {
    /// Pipeline stage, где измерена задержка.
    pub stage: PipelineLatencyStage,

    /// Измеренная задержка.
    pub duration: Duration,

    /// PTS кадра/packet-а, если stage связана с media item.
    pub pts: Option<Duration>,

    /// Memory path кадра, если sample пришёл от decoded frame.
    pub memory_path: Option<FrameMemoryPath>,

    /// Queue depths рядом с sample.
    pub queues: PipelineQueueDepthSnapshot,
}

/// Snapshot latency counters по фиксированному набору stages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineLatencyCountersSnapshot {
    /// Source/demux read latency.
    pub demux_read: LatencyCounterSnapshot,

    /// Decoder packet channel receive latency.
    pub decoder_packet_receive: LatencyCounterSnapshot,

    /// Decoder submit latency.
    pub decoder_submit: LatencyCounterSnapshot,

    /// Decoder event drain latency.
    pub decoder_event_drain: LatencyCounterSnapshot,

    /// Hardware surface sync latency.
    pub hardware_sync: LatencyCounterSnapshot,

    /// DMA-BUF/export latency.
    pub dma_buf_export: LatencyCounterSnapshot,

    /// DMA-BUF/import latency.
    pub dma_buf_import: LatencyCounterSnapshot,

    /// Decoder frame publish latency.
    pub decoded_frame_publish: LatencyCounterSnapshot,

    /// Worker scheduler latency.
    pub worker_scheduler: LatencyCounterSnapshot,

    /// Render acquire wait latency.
    pub render_acquire: LatencyCounterSnapshot,

    /// Wait time на mutex backend resource pool-а внутри renderer materialization boundary.
    pub render_resource_lock_wait: LatencyCounterSnapshot,

    /// GPU submit/present timing, если доступен.
    pub gpu_submit_present: LatencyCounterSnapshot,

    /// Render release acknowledgement latency.
    pub release_acknowledgement: LatencyCounterSnapshot,
}

/// Drop counters по typed причинам.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoDropCountersSnapshot {
    /// Все typed removals, включая ожидаемый seek discard.
    pub total: u64,

    /// Drops вне seek-discard taxonomy: playback, render boundary и pause-state.
    pub playback_or_render: u64,

    /// Ожидаемые discard вокруг seek/generation boundary.
    pub seek_discard: u64,

    /// Late drops.
    pub late: u64,

    /// Queue overflow drops.
    pub queue_overflow: u64,

    /// Stale generation drops.
    pub stale_generation: u64,

    /// Seek/pre-roll drops.
    pub seek_preroll: u64,

    /// Legacy render acquisition timeout drops; non-blocking handoff не должен увеличивать счётчик.
    pub render_acquisition_timeout: u64,

    /// Decoder starvation drops.
    pub decoder_starvation: u64,

    /// Paused-state drops.
    pub paused: u64,

    /// Последний typed drop.
    pub last: Option<VideoDropAttributionSnapshot>,
}

/// Snapshot последнего drop-а с PTS и контекстом очередей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoDropAttributionSnapshot {
    /// PTS удалённого frame/packet-а, если известен.
    pub pts: Option<Duration>,

    /// Typed причина удаления.
    pub reason: VideoDropReason,

    /// Queue depths около удаления.
    pub queues: PipelineQueueDepthSnapshot,
}

/// Diagnostics текущего decoder bootstrap окна после seek/flush.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeekBootstrapDiagnosticsSnapshot {
    /// Сколько video packets были точно inter-frame и отброшены до decode-start packet-а.
    pub dropped_until_keyframe: u64,

    /// Keyframe-состояние первого packet-а, который завершил ожидание decoder bootstrap.
    pub first_accepted_keyframe: Option<PacketKeyframe>,
}

/// Counters временных pipeline pauses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelinePauseCountersSnapshot {
    /// Все typed pauses.
    pub total: u64,

    /// Demux backpressure pauses.
    pub demux_backpressure: u64,

    /// Waiting for free surface/import slot pauses.
    pub waiting_for_free_surface: u64,

    /// Waiting for presentation queue capacity pauses.
    pub waiting_for_present_queue: u64,

    /// Waiting for renderer GPU release pauses.
    pub waiting_for_gpu_release: u64,

    /// Waiting while demux/audio catch-up has priority.
    pub waiting_for_demux_audio_priority: u64,

    /// Decoder packet channel full pauses.
    pub decoder_packet_queue_full: u64,

    /// Decoder control/release channel full pauses.
    pub decoder_control_queue_full: u64,

    /// Software host-upload ready frame queue full pauses.
    pub host_upload_ready_queue_full: u64,

    /// Software host-upload slots exhausted pauses.
    pub host_upload_slots_exhausted: u64,

    /// Decoder starvation pauses.
    pub decoder_starvation: u64,

    /// Scheduler ждёт media sync.
    pub sync_waiting: u64,

    /// Legacy render acquire timeout pauses; non-blocking handoff пишет latency без drop-а.
    pub render_acquire_timeout: u64,

    /// Последняя typed pause.
    pub last: Option<PipelinePauseSnapshot>,
}

/// Snapshot последней pipeline pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelinePauseSnapshot {
    /// Typed причина паузы pipeline.
    pub reason: PipelinePauseReason,

    /// Queue depths около pause.
    pub queues: PipelineQueueDepthSnapshot,
}

/// Snapshot давления на decoder->worker publish boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecoderFramePublishPressureSnapshot {
    /// Сколько раз bounded decoded-frame channel был заполнен.
    pub frame_publish_channel_full_count: u64,

    /// Максимальная latency publish stage среди decoded frames, дошедших до player diagnostics.
    pub max_decoded_frame_publish_latency: Duration,

    /// Суммарная latency publish stage среди decoded frames, дошедших до player diagnostics.
    pub total_decoded_frame_publish_latency: Duration,

    /// Сколько retry attempts сделал decoder thread для pending publish frame.
    pub pending_publish_retry_count: u64,
}

impl DecoderFramePublishPressureSnapshot {
    /// Обновляет channel-pressure counters из decoder-thread event-а.
    fn observe_pressure_event(&mut self, pressure: VideoFramePublishPressureDiagnostics) {
        // Latency totals считаются из `DecodedFrame`: event может прийти раньше frame drain-а.
        self.frame_publish_channel_full_count = pressure.frame_publish_channel_full_count;
        self.pending_publish_retry_count = pressure.pending_publish_retry_count;
    }

    /// Учитывает publish latency ровно один раз после получения decoded frame.
    fn observe_published_frame_latency(&mut self, latency: Duration) {
        self.total_decoded_frame_publish_latency = self
            .total_decoded_frame_publish_latency
            .saturating_add(latency);
        if latency > self.max_decoded_frame_publish_latency {
            self.max_decoded_frame_publish_latency = latency;
        }
    }
}

/// Причина, по которой worker запланировал следующий playback wakeup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerWakeupReason {
    /// Pipeline не требует самостоятельного timeout-а и ждёт внешнего события.
    Idle,

    /// Очереди ниже target или есть demux/decode work, который можно выполнить сразу.
    PipelineWorkReady,

    /// Worker ждёт короткий poll готовности decoder thread-а без привязки к video FPS.
    DecodeReadiness,

    /// Следующий wakeup привязан к PTS первого queued frame.
    FramePtsDeadline,

    /// Первый queued frame уже попадает в окно presentation.
    FrameReady,

    /// Seek/preroll/buffering gate требует быстрого продвижения state machine.
    SeekOrPreroll,

    /// Активный pipeline не дал точного media deadline-а, нужен редкий progress wakeup.
    CoarseProgress,
}

impl WorkerWakeupReason {
    /// Возвращает стабильное имя причины для logs/UI diagnostics.
    #[must_use]
    pub const fn metric_name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::PipelineWorkReady => "pipeline_work_ready",
            Self::DecodeReadiness => "decode_readiness",
            Self::FramePtsDeadline => "frame_pts_deadline",
            Self::FrameReady => "frame_ready",
            Self::SeekOrPreroll => "seek_or_preroll",
            Self::CoarseProgress => "coarse_progress",
        }
    }
}

/// Сравнение первого queued frame с media clock target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerFrameTimingSnapshot {
    /// PTS первого decoded frame в presentation queue.
    pub front_frame_pts: Duration,

    /// Media time, под который scheduler выбирал frame на момент планирования.
    pub target_media_time: Duration,

    /// `front_frame_pts - target_media_time` в микросекундах.
    ///
    /// Положительное значение означает, что frame ещё впереди media clock.
    /// Отрицательное значение означает, что media clock уже прошёл PTS frame-а.
    pub front_frame_delta_from_target_us: i128,
}

/// Последнее решение worker wakeup planner-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkerWakeupDiagnosticsSnapshot {
    /// Почему worker запланировал wakeup.
    pub reason: Option<WorkerWakeupReason>,

    /// Запланированная задержка до wakeup; `None` означает ожидание только событий.
    pub planned_delay: Option<Duration>,

    /// Насколько фактический tick позже запланированного deadline-а.
    pub tick_late_by: Duration,

    /// Сравнение media clock target и первого queued video frame, если он был.
    pub frame_timing: Option<WorkerFrameTimingSnapshot>,
}

/// Текущий blocker активного seek transition-а для логов расследования зависаний.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekProgressBlocker {
    /// Все gates уже готовы, следующий tick должен закрыть seek commit.
    ReadyToCommit,

    /// Audio output ещё не подтвердил очистку buffer-а для текущего seek generation.
    WaitingForAudioClear,

    /// Final resume ждёт минимальный post-seek audio buffer.
    WaitingForAudioPreroll,

    /// Seek ждёт создания decoder-а для выбранного audio track-а.
    WaitingForAudioDecoder,

    /// Seek ждёт создания output-а для выбранного audio track-а.
    WaitingForAudioOutput,

    /// Decoder/demux не может получить surface/import slot.
    WaitingForFreeSurface,

    /// Surface удерживается GPU/render lease/reuse path-ом.
    WaitingForGpuRelease,

    /// Video packet ждёт отправки в decoder thread.
    WaitingForDecoderInput,

    /// Decoder после flush всё ещё ждёт packet, который можно использовать как decode-start.
    WaitingForPostFlushKeyframe,

    /// Packet уже ушёл в decoder, но decoded frame ещё не вернулся.
    WaitingForDecoderOutput,

    /// Demuxer ещё должен прочитать packets после seek.
    WaitingForDemux,

    /// Нужный decoded frame уже около scheduler-а, но ещё не стал present frame.
    WaitingForScheduler,

    /// Первый queued frame уже можно показать без ожидания media clock window.
    ReadyForScheduler,

    /// Target frame ещё не был показан.
    WaitingForVideoTargetFrame,

    /// Target frame показан, но перед resume нужен дополнительный video preroll.
    WaitingForVideoResumePreroll,

    /// Диагностика не смогла свести состояние к более точному blocker-у.
    Unknown,
}

impl SeekProgressBlocker {
    /// Возвращает стабильное имя blocker-а для structured logs.
    #[must_use]
    pub const fn metric_name(self) -> &'static str {
        match self {
            Self::ReadyToCommit => "ready_to_commit",
            Self::WaitingForAudioClear => "audio_clear",
            Self::WaitingForAudioPreroll => "audio_preroll",
            Self::WaitingForAudioDecoder => "audio_decoder",
            Self::WaitingForAudioOutput => "audio_output",
            Self::WaitingForFreeSurface => "free_surface",
            Self::WaitingForGpuRelease => "gpu_release",
            Self::WaitingForDecoderInput => "decoder_input",
            Self::WaitingForPostFlushKeyframe => "post_flush_keyframe",
            Self::WaitingForDecoderOutput => "decoder_output",
            Self::WaitingForDemux => "demux",
            Self::WaitingForScheduler => "scheduler",
            Self::ReadyForScheduler => "ready_for_scheduler",
            Self::WaitingForVideoTargetFrame => "video_target_frame",
            Self::WaitingForVideoResumePreroll => "video_resume_preroll",
            Self::Unknown => "unknown",
        }
    }
}

/// Временные отметки ключевых стадий active Accurate seek preroll-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeekPrerollStageDiagnosticsSnapshot {
    /// Первый demux packet после accepted seek-а.
    pub first_post_seek_packet_elapsed: Option<Duration>,

    /// Первый selected video packet на user target-е или позже.
    pub first_target_or_after_video_packet_elapsed: Option<Duration>,

    /// Первый decoded video frame на user target-е или позже.
    pub first_decoded_target_frame_elapsed: Option<Duration>,

    /// Первый queued video frame на user target-е или позже.
    pub first_queued_target_frame_elapsed: Option<Duration>,

    /// Первый presented video frame, который закрывает Accurate target gate.
    pub first_presented_target_frame_elapsed: Option<Duration>,
}

/// Счётчики demux событий, увиденных во время active Accurate seek preroll-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeekPrerollDemuxEventCountersSnapshot {
    /// Audio packets, которые demuxer вернул после accepted seek-а.
    pub audio_packets: u64,

    /// Video packets, которые demuxer вернул после accepted seek-а.
    pub video_packets: u64,

    /// EOF markers, полученные до закрытия active seek-а.
    pub end_of_stream: u64,

    /// Track-list reset markers, полученные до закрытия active seek-а.
    pub tracks_changed: u64,

    /// Fatal demux read errors, полученные до закрытия active seek-а.
    pub errors: u64,
}

/// Aggregate counters Accurate seek preroll-а без per-packet allocations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeekPrerollCountersSnapshot {
    /// Разбивка demux событий по типам.
    pub demux_events: SeekPrerollDemuxEventCountersSnapshot,

    /// Audio packets, целиком отброшенные до user target-а.
    pub skipped_audio_preroll_packets: u64,

    /// Все current-seek video packets, отправленные decoder-у до закрытия video gate.
    pub seek_video_packets_sent: u64,

    /// Pre-target video packets, отправленные decoder-у в fast-preroll режиме.
    pub video_preroll_packets_sent: u64,

    /// Target-or-after video packets, отправленные decoder-у до первого landing frame.
    pub target_or_after_video_packets_sent: u64,

    /// Pre-target decoded frames, не допущенные в обычный scheduler/output path.
    pub decoded_pre_target_frames_dropped: u64,

    /// Decoder/video admission pauses во время Accurate fast-preroll.
    pub decoder_backpressure_pauses: u64,
}

/// Read-only snapshot seek-preroll diagnostics для active Accurate seek-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccurateSeekPrerollDiagnosticsSnapshot {
    /// `true`, если текущий seek использует Accurate skip/preroll semantics.
    pub active: bool,

    /// Elapsed timings от момента accepted demux seek-а.
    pub stages: SeekPrerollStageDiagnosticsSnapshot,

    /// Bounded aggregate counters без хранения всех packets.
    pub counters: SeekPrerollCountersSnapshot,
}

/// Snapshot активного seek transition-а для throttled worker log-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveSeekDiagnosticsSnapshot {
    /// Тип активного seek transition-а без раскрытия внутреннего enum-а session.
    pub kind: &'static str,

    /// Packet/frame generation, которому принадлежит seek transaction.
    pub generation: u64,

    /// Текущее pipeline generation около diagnostics snapshot-а.
    pub pipeline_generation: u64,

    /// Выбранный video track на момент stall snapshot-а.
    pub selected_video_track_id: Option<TrackId>,

    /// Выбранный audio track на момент stall snapshot-а.
    pub selected_audio_track_id: Option<TrackId>,

    /// Сколько активен seek transaction.
    pub age: Duration,

    /// Цель seek-а на media timeline.
    pub target: Duration,

    /// Фактическая container position после demux seek.
    pub actual: Duration,

    /// Resume intent, сохранённый на момент старта seek transaction-а.
    pub resume_intent: &'static str,

    /// Исходный public seek mode до container-level mapping-а.
    pub seek_mode: crate::SeekMode,

    /// Главный текущий blocker seek progress.
    pub blocker: SeekProgressBlocker,

    /// Готов ли video gate.
    pub video_gate_ready: bool,

    /// Готов ли audio gate.
    pub audio_gate_ready: bool,

    /// Был ли уже показан frame, который закрывает video gate текущего seek-а.
    pub target_frame_presented: bool,

    /// Сколько video frames уже готовы для resume текущей seek policy.
    pub ready_video_frames: usize,

    /// Сколько video frames требуется текущей seek policy.
    pub required_video_frames: usize,

    /// PTS текущего present frame.
    pub present_frame_pts: Option<Duration>,

    /// PTS первого queued frame перед scheduler-ом.
    pub front_queued_frame_pts: Option<Duration>,

    /// Идёт ли demux чтение.
    pub demuxing_active: bool,

    /// Находится ли pipeline в EOF-drain.
    pub draining_after_eof: bool,

    /// Timeline всё ещё помечает картинку как stale.
    pub stale_frame: bool,

    /// Сколько stale-generation drops уже накоплено в diagnostics.
    pub stale_generation_discards: u64,

    /// Diagnostics текущего decoder bootstrap окна после seek/flush.
    pub seek_bootstrap: SeekBootstrapDiagnosticsSnapshot,

    /// Последняя typed pause-причина, если pipeline уже её зафиксировал.
    pub last_pause_reason: Option<PipelinePauseReason>,

    /// Stage/counter diagnostics Accurate seek preroll-а.
    pub accurate_preroll: AccurateSeekPrerollDiagnosticsSnapshot,

    /// Queue/resource depths около active seek.
    pub queues: PipelineQueueDepthSnapshot,
}

/// Read-only diagnostics snapshot, который UI может только отображать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackDiagnosticsSnapshot {
    /// Последний observed video memory path.
    pub zero_copy_memory_path: Option<FrameMemoryPath>,

    /// Последние queue depths.
    pub queues: PipelineQueueDepthSnapshot,

    /// Typed drop counters.
    pub drops: VideoDropCountersSnapshot,

    /// Diagnostics decoder bootstrap окна после seek/flush.
    pub seek_bootstrap: SeekBootstrapDiagnosticsSnapshot,

    /// Typed pipeline pause counters.
    pub pauses: PipelinePauseCountersSnapshot,

    /// Worst/average latency counters по stage.
    pub worst_latencies: PipelineLatencyCountersSnapshot,

    /// Bounded recent samples, которые обновляли worst latency.
    pub recent_worst_samples: Vec<PipelineLatencySampleSnapshot>,

    /// Количество decoded frames, прошедших через player diagnostics.
    pub decoded_frames: u64,

    /// Количество повторов текущего present frame, не смешанное с media drops.
    pub repeated_video_frames: u64,

    /// Сколько раз non-blocking renderer resource lookup встретил занятый backend lock.
    pub render_resource_lock_busy_count: u64,

    /// Сколько раз renderer переиспользовал previous valid frame из-за busy lock-а.
    pub render_resource_previous_frame_reuse_count: u64,

    /// Давление на bounded decoder->worker decoded-frame publish channel.
    pub decoder_frame_publish_pressure: DecoderFramePublishPressureSnapshot,

    /// Последнее решение worker wakeup planner-а.
    pub worker_wakeup: WorkerWakeupDiagnosticsSnapshot,

    /// Neutral frame-server/player scrub diagnostics без UI строк и history.
    pub frame_server_scrub: ScrubDiagnosticsSnapshot,
}

impl Default for PlaybackDiagnosticsSnapshot {
    /// Возвращает пустой snapshot без heap churn в hot path.
    fn default() -> Self {
        Self {
            zero_copy_memory_path: None,
            queues: PipelineQueueDepthSnapshot::default(),
            drops: VideoDropCountersSnapshot::default(),
            seek_bootstrap: SeekBootstrapDiagnosticsSnapshot::default(),
            pauses: PipelinePauseCountersSnapshot::default(),
            worst_latencies: PipelineLatencyCountersSnapshot::default(),
            recent_worst_samples: Vec::new(),
            decoded_frames: 0,
            repeated_video_frames: 0,
            render_resource_lock_busy_count: 0,
            render_resource_previous_frame_reuse_count: 0,
            decoder_frame_publish_pressure: DecoderFramePublishPressureSnapshot::default(),
            worker_wakeup: WorkerWakeupDiagnosticsSnapshot::default(),
            frame_server_scrub: ScrubDiagnosticsSnapshot::new(),
        }
    }
}

/// Компактная log-сводка без строковых аллокаций на hot path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaybackDiagnosticsLogSummary {
    /// Все typed drops.
    pub drops_total: u64,

    /// Drop counters по причинам.
    pub drops: VideoDropCountersSnapshot,

    /// Diagnostics decoder bootstrap окна после seek/flush.
    pub seek_bootstrap: SeekBootstrapDiagnosticsSnapshot,

    /// Все typed pauses.
    pub pauses_total: u64,

    /// Pipeline pause counters по причинам.
    pub pauses: PipelinePauseCountersSnapshot,

    /// Последний observed memory path.
    pub zero_copy_memory_path: Option<FrameMemoryPath>,

    /// Количество repeats, которые не являются media drops.
    pub repeated_video_frames: u64,

    /// Сколько раз non-blocking renderer resource lookup встретил занятый backend lock.
    pub render_resource_lock_busy_count: u64,

    /// Сколько раз renderer переиспользовал previous valid frame из-за busy lock-а.
    pub render_resource_previous_frame_reuse_count: u64,

    /// Давление на bounded decoder->worker decoded-frame publish channel.
    pub decoder_frame_publish_pressure: DecoderFramePublishPressureSnapshot,

    /// Последнее решение worker wakeup planner-а.
    pub worker_wakeup: WorkerWakeupDiagnosticsSnapshot,

    /// Самая медленная stage на момент summary.
    pub worst_stage: Option<PipelineLatencyStage>,

    /// Worst latency самой медленной stage.
    pub worst_latency: Option<Duration>,

    /// Latency counters по всем фиксированным stage.
    pub worst_latencies: PipelineLatencyCountersSnapshot,

    /// Последние queue depths.
    pub queues: PipelineQueueDepthSnapshot,
}

impl PlaybackDiagnosticsLogSummary {
    /// Возвращает `true`, если summary содержит полезную runtime активность.
    #[must_use]
    pub const fn has_activity(self) -> bool {
        self.drops_total > 0
            || self.pauses_total > 0
            || self.seek_bootstrap.dropped_until_keyframe > 0
            || self.seek_bootstrap.first_accepted_keyframe.is_some()
            || self.render_resource_lock_busy_count > 0
            || self.render_resource_previous_frame_reuse_count > 0
            || self
                .decoder_frame_publish_pressure
                .frame_publish_channel_full_count
                > 0
            || self
                .decoder_frame_publish_pressure
                .pending_publish_retry_count
                > 0
            || match self.queues.decoder_control_channel {
                Some(pressure) => {
                    pressure.control_channel_full_count > 0
                        || pressure.release_control_send_fail_count > 0
                        || pressure.flush_control_send_fail_count > 0
                }
                None => false,
            }
            || self.zero_copy_memory_path.is_some()
            || self.worst_latency.is_some()
    }
}

/// Runtime aggregator diagnostics внутри player-core.
#[derive(Debug, Clone)]
pub(crate) struct PlaybackDiagnostics {
    /// Последний published snapshot без cloning unbounded state.
    snapshot: PlaybackDiagnosticsSnapshot,

    /// Recent worst samples как bounded ring.
    recent_worst_samples: VecDeque<PipelineLatencySampleSnapshot>,

    /// Mutable latency counters.
    latency_counters: PipelineLatencyCounters,

    /// Neutral scrub diagnostics recorder; хранит только bounded counters/snapshots.
    frame_server_scrub: ScrubDiagnosticsRecorder,
}

impl PlaybackDiagnostics {
    /// Создаёт пустой bounded aggregator.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            snapshot: PlaybackDiagnosticsSnapshot::default(),
            recent_worst_samples: VecDeque::with_capacity(RECENT_WORST_SAMPLE_LIMIT),
            latency_counters: PipelineLatencyCounters::default(),
            frame_server_scrub: ScrubDiagnosticsRecorder::new(),
        }
    }

    /// Сбрасывает media-specific diagnostics.
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    /// Начинает новое post-flush decoder bootstrap окно и сбрасывает per-window counters.
    pub(crate) fn start_seek_bootstrap(&mut self, queues: PipelineQueueDepthSnapshot) {
        self.snapshot.seek_bootstrap = SeekBootstrapDiagnosticsSnapshot::default();
        self.snapshot.queues = queues;
    }

    /// Учитывает packet, отброшенный потому, что decoder после flush ждёт decode-start.
    pub(crate) fn record_seek_bootstrap_drop_until_keyframe(
        &mut self,
        queues: PipelineQueueDepthSnapshot,
    ) -> SeekBootstrapDiagnosticsSnapshot {
        self.snapshot.seek_bootstrap.dropped_until_keyframe = self
            .snapshot
            .seek_bootstrap
            .dropped_until_keyframe
            .saturating_add(1);
        self.snapshot.queues = queues;
        self.snapshot.seek_bootstrap
    }

    /// Запоминает первый packet, который завершил ожидание post-flush decode-start.
    pub(crate) fn record_seek_bootstrap_first_accepted(
        &mut self,
        keyframe: PacketKeyframe,
        queues: PipelineQueueDepthSnapshot,
    ) -> SeekBootstrapDiagnosticsSnapshot {
        if self
            .snapshot
            .seek_bootstrap
            .first_accepted_keyframe
            .is_none()
        {
            self.snapshot.seek_bootstrap.first_accepted_keyframe = Some(keyframe);
        }

        self.snapshot.queues = queues;
        self.snapshot.seek_bootstrap
    }

    /// Записывает decoded frame и его backend-provided timings.
    pub(crate) fn observe_decoded_frame(
        &mut self,
        frame: &DecodedFrame,
        queues: PipelineQueueDepthSnapshot,
    ) {
        self.snapshot.decoded_frames = self.snapshot.decoded_frames.saturating_add(1);
        let memory_path = frame.memory_path();
        self.snapshot.zero_copy_memory_path = Some(memory_path);
        self.snapshot.queues = queues;

        self.record_optional_latency(
            PipelineLatencyStage::DecoderPacketReceive,
            frame.diagnostics.timings.decoder_packet_receive_latency,
            Some(frame.pts),
            Some(memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::DecoderSubmit,
            frame.diagnostics.timings.decoder_submit_latency,
            Some(frame.pts),
            Some(memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::DecoderEventDrain,
            frame.diagnostics.timings.decoder_event_drain_latency,
            Some(frame.pts),
            Some(memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::HardwareSync,
            frame.diagnostics.timings.hardware_sync_latency,
            Some(frame.pts),
            Some(memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::DmaBufExport,
            frame.diagnostics.timings.dma_buf_export_latency,
            Some(frame.pts),
            Some(memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::DmaBufImport,
            frame.diagnostics.timings.dma_buf_import_latency,
            Some(frame.pts),
            Some(memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::DecodedFramePublish,
            frame.diagnostics.timings.decoded_frame_publish_latency,
            Some(frame.pts),
            Some(memory_path),
            queues,
        );
        if let Some(latency) = frame.diagnostics.timings.decoded_frame_publish_latency {
            self.snapshot
                .decoder_frame_publish_pressure
                .observe_published_frame_latency(latency);
        }
    }

    /// Записывает pressure counters, пришедшие от decoder-thread publish boundary.
    pub(crate) fn record_decoded_frame_publish_pressure(
        &mut self,
        pressure: VideoFramePublishPressureDiagnostics,
        queues: PipelineQueueDepthSnapshot,
    ) {
        self.snapshot
            .decoder_frame_publish_pressure
            .observe_pressure_event(pressure);
        self.snapshot.queues = queues;
    }

    /// Записывает latency sample конкретной stage.
    pub(crate) fn record_latency(
        &mut self,
        stage: PipelineLatencyStage,
        duration: Duration,
        pts: Option<Duration>,
        memory_path: Option<FrameMemoryPath>,
        queues: PipelineQueueDepthSnapshot,
    ) {
        let sample = PipelineLatencySampleSnapshot {
            stage,
            duration,
            pts,
            memory_path,
            queues,
        };
        let is_new_worst = self.latency_counters.record(sample);

        if is_new_worst {
            self.push_recent_worst_sample(sample);
        }

        self.snapshot.worst_latencies = self.latency_counters.snapshot();
        self.snapshot.queues = queues;
    }

    /// Записывает typed drop reason.
    pub(crate) fn record_drop(
        &mut self,
        pts: Option<Duration>,
        reason: VideoDropReason,
        queues: PipelineQueueDepthSnapshot,
    ) {
        self.snapshot.drops.total = self.snapshot.drops.total.saturating_add(1);
        match reason {
            VideoDropReason::Late => {
                self.snapshot.drops.playback_or_render =
                    self.snapshot.drops.playback_or_render.saturating_add(1);
                self.snapshot.drops.late = self.snapshot.drops.late.saturating_add(1);
            }
            VideoDropReason::QueueOverflow => {
                self.snapshot.drops.playback_or_render =
                    self.snapshot.drops.playback_or_render.saturating_add(1);
                self.snapshot.drops.queue_overflow =
                    self.snapshot.drops.queue_overflow.saturating_add(1);
            }
            VideoDropReason::StaleGeneration => {
                self.snapshot.drops.seek_discard =
                    self.snapshot.drops.seek_discard.saturating_add(1);
                self.snapshot.drops.stale_generation =
                    self.snapshot.drops.stale_generation.saturating_add(1);
            }
            VideoDropReason::SeekPreroll => {
                self.snapshot.drops.seek_discard =
                    self.snapshot.drops.seek_discard.saturating_add(1);
                self.snapshot.drops.seek_preroll =
                    self.snapshot.drops.seek_preroll.saturating_add(1);
            }
            VideoDropReason::RenderAcquisitionTimeout => {
                self.snapshot.drops.playback_or_render =
                    self.snapshot.drops.playback_or_render.saturating_add(1);
                self.snapshot.drops.render_acquisition_timeout = self
                    .snapshot
                    .drops
                    .render_acquisition_timeout
                    .saturating_add(1);
            }
            VideoDropReason::DecoderStarvation => {
                self.snapshot.drops.playback_or_render =
                    self.snapshot.drops.playback_or_render.saturating_add(1);
                self.snapshot.drops.decoder_starvation =
                    self.snapshot.drops.decoder_starvation.saturating_add(1);
            }
            VideoDropReason::Paused => {
                self.snapshot.drops.playback_or_render =
                    self.snapshot.drops.playback_or_render.saturating_add(1);
                self.snapshot.drops.paused = self.snapshot.drops.paused.saturating_add(1);
            }
        }
        self.snapshot.drops.last = Some(VideoDropAttributionSnapshot {
            pts,
            reason,
            queues,
        });
        self.snapshot.queues = queues;
    }

    /// Записывает typed pipeline pause.
    pub(crate) fn record_pause(
        &mut self,
        reason: PipelinePauseReason,
        queues: PipelineQueueDepthSnapshot,
    ) {
        self.snapshot.pauses.total = self.snapshot.pauses.total.saturating_add(1);
        match reason {
            PipelinePauseReason::DemuxBackpressure => {
                self.snapshot.pauses.demux_backpressure =
                    self.snapshot.pauses.demux_backpressure.saturating_add(1);
            }
            PipelinePauseReason::WaitingForFreeSurface => {
                self.snapshot.pauses.waiting_for_free_surface = self
                    .snapshot
                    .pauses
                    .waiting_for_free_surface
                    .saturating_add(1);
            }
            PipelinePauseReason::WaitingForPresentQueue => {
                self.snapshot.pauses.waiting_for_present_queue = self
                    .snapshot
                    .pauses
                    .waiting_for_present_queue
                    .saturating_add(1);
            }
            PipelinePauseReason::WaitingForGpuRelease => {
                self.snapshot.pauses.waiting_for_gpu_release = self
                    .snapshot
                    .pauses
                    .waiting_for_gpu_release
                    .saturating_add(1);
            }
            PipelinePauseReason::WaitingForDemuxAudioPriority => {
                self.snapshot.pauses.waiting_for_demux_audio_priority = self
                    .snapshot
                    .pauses
                    .waiting_for_demux_audio_priority
                    .saturating_add(1);
            }
            PipelinePauseReason::DecoderPacketQueueFull => {
                self.snapshot.pauses.decoder_packet_queue_full = self
                    .snapshot
                    .pauses
                    .decoder_packet_queue_full
                    .saturating_add(1);
            }
            PipelinePauseReason::DecoderControlQueueFull => {
                self.snapshot.pauses.decoder_control_queue_full = self
                    .snapshot
                    .pauses
                    .decoder_control_queue_full
                    .saturating_add(1);
            }
            PipelinePauseReason::HostUploadReadyQueueFull => {
                self.snapshot.pauses.host_upload_ready_queue_full = self
                    .snapshot
                    .pauses
                    .host_upload_ready_queue_full
                    .saturating_add(1);
            }
            PipelinePauseReason::HostUploadSlotsExhausted => {
                self.snapshot.pauses.host_upload_slots_exhausted = self
                    .snapshot
                    .pauses
                    .host_upload_slots_exhausted
                    .saturating_add(1);
            }
            PipelinePauseReason::DecoderStarvation => {
                self.snapshot.pauses.decoder_starvation =
                    self.snapshot.pauses.decoder_starvation.saturating_add(1);
            }
            PipelinePauseReason::SyncWaiting => {
                self.snapshot.pauses.sync_waiting =
                    self.snapshot.pauses.sync_waiting.saturating_add(1);
            }
            PipelinePauseReason::RenderAcquireTimeout => {
                self.snapshot.pauses.render_acquire_timeout = self
                    .snapshot
                    .pauses
                    .render_acquire_timeout
                    .saturating_add(1);
            }
        }
        self.snapshot.pauses.last = Some(PipelinePauseSnapshot { reason, queues });
        self.snapshot.queues = queues;
    }

    /// Записывает повтор текущего frame как отдельную pacing telemetry, не как drop.
    pub(crate) fn record_repeated_video_frame(&mut self, queues: PipelineQueueDepthSnapshot) {
        self.snapshot.repeated_video_frames = self.snapshot.repeated_video_frames.saturating_add(1);
        self.snapshot.queues = queues;
    }

    /// Записывает busy outcome non-blocking renderer resource lookup-а.
    pub(crate) fn record_render_resource_lock_busy(&mut self, queues: PipelineQueueDepthSnapshot) {
        self.snapshot.render_resource_lock_busy_count = self
            .snapshot
            .render_resource_lock_busy_count
            .saturating_add(1);
        self.snapshot.queues = queues;
    }

    /// Записывает reuse previous frame-а из-за busy renderer resource lock-а.
    pub(crate) fn record_render_resource_previous_frame_reuse(
        &mut self,
        queues: PipelineQueueDepthSnapshot,
    ) {
        self.snapshot.render_resource_previous_frame_reuse_count = self
            .snapshot
            .render_resource_previous_frame_reuse_count
            .saturating_add(1);
        self.snapshot.queues = queues;
    }

    /// Записывает последнее решение worker wakeup planner-а.
    pub(crate) fn record_worker_wakeup(
        &mut self,
        wakeup: WorkerWakeupDiagnosticsSnapshot,
        queues: PipelineQueueDepthSnapshot,
    ) {
        self.snapshot.worker_wakeup = wakeup;
        self.snapshot.queues = queues;
    }

    pub(crate) fn record_scrub_event_diagnostics(&mut self, diagnostics: ScrubEventDiagnostics) {
        self.frame_server_scrub
            .record_event_diagnostics(diagnostics);
    }

    /// Возвращает snapshot с актуальными queue depths.
    #[must_use]
    pub(crate) fn snapshot_with_queues(
        &self,
        queues: PipelineQueueDepthSnapshot,
    ) -> PlaybackDiagnosticsSnapshot {
        let mut snapshot = self.snapshot.clone();
        snapshot.queues = queues;
        snapshot.recent_worst_samples = self.recent_worst_samples.iter().copied().collect();
        snapshot.frame_server_scrub = self.frame_server_scrub.snapshot();
        snapshot
    }

    /// Возвращает компактную log-сводку.
    #[must_use]
    pub(crate) fn log_summary(
        &self,
        queues: PipelineQueueDepthSnapshot,
    ) -> PlaybackDiagnosticsLogSummary {
        let (worst_stage, worst_latency) = self.latency_counters.global_worst();
        PlaybackDiagnosticsLogSummary {
            drops_total: self.snapshot.drops.total,
            drops: self.snapshot.drops,
            seek_bootstrap: self.snapshot.seek_bootstrap,
            pauses_total: self.snapshot.pauses.total,
            pauses: self.snapshot.pauses,
            zero_copy_memory_path: self.snapshot.zero_copy_memory_path,
            repeated_video_frames: self.snapshot.repeated_video_frames,
            render_resource_lock_busy_count: self.snapshot.render_resource_lock_busy_count,
            render_resource_previous_frame_reuse_count: self
                .snapshot
                .render_resource_previous_frame_reuse_count,
            decoder_frame_publish_pressure: self.snapshot.decoder_frame_publish_pressure,
            worker_wakeup: self.snapshot.worker_wakeup,
            worst_stage,
            worst_latency,
            worst_latencies: self.latency_counters.snapshot(),
            queues,
        }
    }

    /// Записывает optional latency без ветвления в вызывающем коде.
    fn record_optional_latency(
        &mut self,
        stage: PipelineLatencyStage,
        duration: Option<Duration>,
        pts: Option<Duration>,
        memory_path: Option<FrameMemoryPath>,
        queues: PipelineQueueDepthSnapshot,
    ) {
        if let Some(duration) = duration {
            self.record_latency(stage, duration, pts, memory_path, queues);
        }
    }

    /// Добавляет recent worst sample, сохраняя фиксированный лимит.
    fn push_recent_worst_sample(&mut self, sample: PipelineLatencySampleSnapshot) {
        if self.recent_worst_samples.len() == RECENT_WORST_SAMPLE_LIMIT {
            self.recent_worst_samples.pop_front();
        }
        self.recent_worst_samples.push_back(sample);
    }
}

impl Default for PlaybackDiagnostics {
    /// Создаёт bounded diagnostics aggregator.
    fn default() -> Self {
        Self::new()
    }
}

/// Mutable counters по stage.
#[derive(Debug, Clone, Default)]
struct PipelineLatencyCounters {
    /// Source/demux read.
    demux_read: LatencyCounter,

    /// Decoder packet receive.
    decoder_packet_receive: LatencyCounter,

    /// Decoder submit.
    decoder_submit: LatencyCounter,

    /// Decoder event drain.
    decoder_event_drain: LatencyCounter,

    /// Hardware surface sync.
    hardware_sync: LatencyCounter,

    /// DMA-BUF export.
    dma_buf_export: LatencyCounter,

    /// DMA-BUF import.
    dma_buf_import: LatencyCounter,

    /// Decoded frame publish.
    decoded_frame_publish: LatencyCounter,

    /// Worker scheduler.
    worker_scheduler: LatencyCounter,

    /// Render acquire.
    render_acquire: LatencyCounter,

    /// Renderer resource mutex wait.
    render_resource_lock_wait: LatencyCounter,

    /// GPU submit/present.
    gpu_submit_present: LatencyCounter,

    /// Release ack.
    release_acknowledgement: LatencyCounter,
}

impl PipelineLatencyCounters {
    /// Записывает sample и возвращает `true`, если он стал worst для stage.
    fn record(&mut self, sample: PipelineLatencySampleSnapshot) -> bool {
        self.counter_mut(sample.stage).record(sample)
    }

    /// Собирает immutable snapshot counters.
    fn snapshot(&self) -> PipelineLatencyCountersSnapshot {
        PipelineLatencyCountersSnapshot {
            demux_read: self.demux_read.snapshot(),
            decoder_packet_receive: self.decoder_packet_receive.snapshot(),
            decoder_submit: self.decoder_submit.snapshot(),
            decoder_event_drain: self.decoder_event_drain.snapshot(),
            hardware_sync: self.hardware_sync.snapshot(),
            dma_buf_export: self.dma_buf_export.snapshot(),
            dma_buf_import: self.dma_buf_import.snapshot(),
            decoded_frame_publish: self.decoded_frame_publish.snapshot(),
            worker_scheduler: self.worker_scheduler.snapshot(),
            render_acquire: self.render_acquire.snapshot(),
            render_resource_lock_wait: self.render_resource_lock_wait.snapshot(),
            gpu_submit_present: self.gpu_submit_present.snapshot(),
            release_acknowledgement: self.release_acknowledgement.snapshot(),
        }
    }

    /// Возвращает глобально худшую stage.
    fn global_worst(&self) -> (Option<PipelineLatencyStage>, Option<Duration>) {
        let mut worst_stage = None;
        let mut worst_latency = None;

        for (stage, counter) in self.stage_counters() {
            let Some(sample) = counter.worst else {
                continue;
            };
            if worst_latency.is_none_or(|latency| sample.duration > latency) {
                worst_stage = Some(stage);
                worst_latency = Some(sample.duration);
            }
        }

        (worst_stage, worst_latency)
    }

    /// Возвращает mutable counter для stage.
    fn counter_mut(&mut self, stage: PipelineLatencyStage) -> &mut LatencyCounter {
        match stage {
            PipelineLatencyStage::DemuxRead => &mut self.demux_read,
            PipelineLatencyStage::DecoderPacketReceive => &mut self.decoder_packet_receive,
            PipelineLatencyStage::DecoderSubmit => &mut self.decoder_submit,
            PipelineLatencyStage::DecoderEventDrain => &mut self.decoder_event_drain,
            PipelineLatencyStage::HardwareSync => &mut self.hardware_sync,
            PipelineLatencyStage::DmaBufExport => &mut self.dma_buf_export,
            PipelineLatencyStage::DmaBufImport => &mut self.dma_buf_import,
            PipelineLatencyStage::DecodedFramePublish => &mut self.decoded_frame_publish,
            PipelineLatencyStage::WorkerScheduler => &mut self.worker_scheduler,
            PipelineLatencyStage::RenderAcquire => &mut self.render_acquire,
            PipelineLatencyStage::RenderResourceLockWait => &mut self.render_resource_lock_wait,
            PipelineLatencyStage::GpuSubmitPresent => &mut self.gpu_submit_present,
            PipelineLatencyStage::ReleaseAcknowledgement => &mut self.release_acknowledgement,
        }
    }

    /// Возвращает fixed list stage counters для summary.
    fn stage_counters(&self) -> [(PipelineLatencyStage, &LatencyCounter); 13] {
        [
            (PipelineLatencyStage::DemuxRead, &self.demux_read),
            (
                PipelineLatencyStage::DecoderPacketReceive,
                &self.decoder_packet_receive,
            ),
            (PipelineLatencyStage::DecoderSubmit, &self.decoder_submit),
            (
                PipelineLatencyStage::DecoderEventDrain,
                &self.decoder_event_drain,
            ),
            (PipelineLatencyStage::HardwareSync, &self.hardware_sync),
            (PipelineLatencyStage::DmaBufExport, &self.dma_buf_export),
            (PipelineLatencyStage::DmaBufImport, &self.dma_buf_import),
            (
                PipelineLatencyStage::DecodedFramePublish,
                &self.decoded_frame_publish,
            ),
            (
                PipelineLatencyStage::WorkerScheduler,
                &self.worker_scheduler,
            ),
            (PipelineLatencyStage::RenderAcquire, &self.render_acquire),
            (
                PipelineLatencyStage::RenderResourceLockWait,
                &self.render_resource_lock_wait,
            ),
            (
                PipelineLatencyStage::GpuSubmitPresent,
                &self.gpu_submit_present,
            ),
            (
                PipelineLatencyStage::ReleaseAcknowledgement,
                &self.release_acknowledgement,
            ),
        ]
    }
}

/// Mutable latency counter одной stage.
#[derive(Debug, Clone, Copy, Default)]
struct LatencyCounter {
    /// Samples count.
    samples: u64,

    /// Saturating суммарная latency.
    total: Duration,

    /// Worst sample.
    worst: Option<PipelineLatencySampleSnapshot>,
}

impl LatencyCounter {
    /// Записывает sample и возвращает `true`, если он стал worst.
    fn record(&mut self, sample: PipelineLatencySampleSnapshot) -> bool {
        self.samples = self.samples.saturating_add(1);
        self.total = self.total.saturating_add(sample.duration);

        let is_new_worst = self
            .worst
            .is_none_or(|worst_sample| sample.duration >= worst_sample.duration);
        if is_new_worst {
            self.worst = Some(sample);
        }
        is_new_worst
    }

    /// Собирает immutable snapshot.
    fn snapshot(self) -> LatencyCounterSnapshot {
        LatencyCounterSnapshot {
            samples: self.samples,
            average: average_duration(self.total, self.samples),
            worst: self.worst,
        }
    }
}

/// Делит Duration на количество samples без panic и переполнения.
fn average_duration(total: Duration, samples: u64) -> Duration {
    if samples == 0 {
        return Duration::ZERO;
    }

    let average_nanos = total.as_nanos() / u128::from(samples);
    Duration::from_nanos(average_nanos.min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec_core::VideoColorMetadata;
    use video_core::{FrameResourceHandle, VideoFrameTimingDiagnostics};
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    /// Создаёт queue snapshot для чистых aggregation тестов.
    fn queue_depths_for_tests(pending_video_packets: usize) -> PipelineQueueDepthSnapshot {
        PipelineQueueDepthSnapshot {
            pending_video_packets,
            present_queue_depth: 2,
            decoder_send_queue_depth: pending_video_packets,
            decoder_ready_queue_depth: Some(1),
            texture_slots: Some(TextureSlotPressureSnapshot {
                capacity: 16,
                slots: 4,
                in_use: 3,
                free_surfaces: 1,
                waiting_gpu_completion: 1,
                waiting_decoder_reuse: 0,
                import_failures: 0,
                imports_created: 4,
                imports_reused: 8,
                imports_replaced: 0,
            }),
            ..PipelineQueueDepthSnapshot::default()
        }
    }

    /// Создаёт decoded frame с diagnostics timings без GPU handles.
    fn decoded_frame_for_tests() -> DecodedFrame {
        DecodedFrame {
            generation: 0,
            pts: Duration::from_millis(42),
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(7),
            diagnostics: video_core::VideoFrameDiagnostics {
                timings: VideoFrameTimingDiagnostics {
                    decoder_submit_latency: Some(Duration::from_millis(3)),
                    dma_buf_import_latency: Some(Duration::from_millis(5)),
                    ..VideoFrameTimingDiagnostics::default()
                },
                decoder_ready_queue_depth: Some(1),
                resource_pool: None,
            },
        }
    }

    #[test]
    fn aggregation_maps_drop_reasons_to_typed_counters() {
        let mut diagnostics = PlaybackDiagnostics::new();
        let queues = queue_depths_for_tests(3);

        diagnostics.record_drop(
            Some(Duration::from_millis(10)),
            VideoDropReason::SeekPreroll,
            queues,
        );
        diagnostics.record_drop(
            Some(Duration::from_millis(20)),
            VideoDropReason::StaleGeneration,
            queues,
        );
        diagnostics.record_drop(
            Some(Duration::from_millis(30)),
            VideoDropReason::Late,
            queues,
        );
        diagnostics.record_drop(
            Some(Duration::from_millis(40)),
            VideoDropReason::RenderAcquisitionTimeout,
            queues,
        );

        let snapshot = diagnostics.snapshot_with_queues(queues);

        assert_eq!(snapshot.drops.total, 4);
        assert_eq!(snapshot.drops.seek_discard, 2);
        assert_eq!(snapshot.drops.playback_or_render, 2);
        assert_eq!(snapshot.drops.seek_preroll, 1);
        assert_eq!(snapshot.drops.stale_generation, 1);
        assert_eq!(snapshot.drops.late, 1);
        assert_eq!(snapshot.drops.render_acquisition_timeout, 1);
        assert_eq!(
            snapshot.drops.last.map(|drop| drop.reason),
            Some(VideoDropReason::RenderAcquisitionTimeout)
        );
    }

    #[test]
    fn aggregation_tracks_seek_bootstrap_window() {
        let mut diagnostics = PlaybackDiagnostics::new();
        let queues = queue_depths_for_tests(2);

        diagnostics.start_seek_bootstrap(queues);
        diagnostics.record_seek_bootstrap_drop_until_keyframe(queues);
        diagnostics.record_seek_bootstrap_drop_until_keyframe(queues);
        diagnostics.record_seek_bootstrap_first_accepted(PacketKeyframe::Unknown, queues);

        let snapshot = diagnostics.snapshot_with_queues(queues);

        assert_eq!(snapshot.seek_bootstrap.dropped_until_keyframe, 2);
        assert_eq!(
            snapshot.seek_bootstrap.first_accepted_keyframe,
            Some(PacketKeyframe::Unknown)
        );

        diagnostics.start_seek_bootstrap(queues);

        assert_eq!(
            diagnostics.snapshot_with_queues(queues).seek_bootstrap,
            SeekBootstrapDiagnosticsSnapshot::default()
        );
    }

    #[test]
    fn aggregation_records_frame_timings_without_unbounded_samples() {
        let mut diagnostics = PlaybackDiagnostics::new();
        let queues = queue_depths_for_tests(1);
        let frame = decoded_frame_for_tests();

        diagnostics.observe_decoded_frame(&frame, queues);

        let snapshot = diagnostics.snapshot_with_queues(queues);

        assert_eq!(
            snapshot.zero_copy_memory_path,
            Some(FrameMemoryPath::DmaBufZeroCopy)
        );
        assert_eq!(snapshot.decoded_frames, 1);
        assert_eq!(snapshot.worst_latencies.decoder_submit.samples, 1);
        assert_eq!(snapshot.worst_latencies.dma_buf_import.samples, 1);
        assert!(snapshot.recent_worst_samples.len() <= RECENT_WORST_SAMPLE_LIMIT);
    }

    #[test]
    fn aggregation_records_decoder_frame_publish_pressure_snapshot() {
        let mut diagnostics = PlaybackDiagnostics::new();
        let queues = queue_depths_for_tests(2);
        let pressure = VideoFramePublishPressureDiagnostics {
            frame_publish_channel_full_count: 3,
            pending_publish_retry_count: 2,
            max_decoded_frame_publish_latency: Duration::from_millis(99),
            total_decoded_frame_publish_latency: Duration::from_millis(150),
        };
        let mut first_frame = decoded_frame_for_tests();
        first_frame
            .diagnostics
            .timings
            .decoded_frame_publish_latency = Some(Duration::from_millis(4));
        let mut second_frame = decoded_frame_for_tests();
        second_frame
            .diagnostics
            .timings
            .decoded_frame_publish_latency = Some(Duration::from_millis(6));

        diagnostics.record_decoded_frame_publish_pressure(pressure, queues);
        diagnostics.observe_decoded_frame(&first_frame, queues);
        diagnostics.observe_decoded_frame(&second_frame, queues);

        let snapshot = diagnostics.snapshot_with_queues(queues);
        let publish_pressure = snapshot.decoder_frame_publish_pressure;

        assert_eq!(publish_pressure.frame_publish_channel_full_count, 3);
        assert_eq!(publish_pressure.pending_publish_retry_count, 2);
        assert_eq!(
            publish_pressure.max_decoded_frame_publish_latency,
            Duration::from_millis(6)
        );
        assert_eq!(
            publish_pressure.total_decoded_frame_publish_latency,
            Duration::from_millis(10)
        );
        assert_eq!(snapshot.worst_latencies.decoded_frame_publish.samples, 2);
    }

    #[test]
    fn log_summary_treats_decoder_control_pressure_as_activity() {
        let diagnostics = PlaybackDiagnostics::new();
        let mut queues = queue_depths_for_tests(0);
        queues.decoder_control_channel = Some(DecoderControlChannelPressureSnapshot {
            control_channel_len: 32,
            control_channel_capacity: 32,
            control_channel_full_count: 1,
            release_control_send_fail_count: 1,
            flush_control_send_fail_count: 0,
        });

        let summary = diagnostics.log_summary(queues);
        let control_pressure = summary
            .queues
            .decoder_control_channel
            .expect("control pressure snapshot should be preserved in log summary");

        assert!(summary.has_activity());
        assert_eq!(control_pressure.control_channel_len, 32);
        assert_eq!(control_pressure.control_channel_capacity, 32);
        assert_eq!(control_pressure.control_channel_full_count, 1);
        assert_eq!(control_pressure.release_control_send_fail_count, 1);
        assert_eq!(control_pressure.flush_control_send_fail_count, 0);
    }

    #[test]
    fn recent_worst_samples_are_bounded() {
        let mut diagnostics = PlaybackDiagnostics::new();
        let queues = queue_depths_for_tests(0);

        for milliseconds in 0..64 {
            diagnostics.record_latency(
                PipelineLatencyStage::DemuxRead,
                Duration::from_millis(milliseconds),
                None,
                None,
                queues,
            );
        }

        let snapshot = diagnostics.snapshot_with_queues(queues);

        assert_eq!(
            snapshot.recent_worst_samples.len(),
            RECENT_WORST_SAMPLE_LIMIT
        );
        assert_eq!(
            snapshot
                .recent_worst_samples
                .last()
                .map(|sample| sample.duration),
            Some(Duration::from_millis(63))
        );
    }

    #[test]
    fn render_resource_lock_wait_latency_has_count_average_and_worst() {
        let mut diagnostics = PlaybackDiagnostics::new();
        let queues = queue_depths_for_tests(0);

        diagnostics.record_latency(
            PipelineLatencyStage::RenderResourceLockWait,
            Duration::from_micros(100),
            Some(Duration::from_millis(40)),
            Some(FrameMemoryPath::DmaBufZeroCopy),
            queues,
        );
        diagnostics.record_latency(
            PipelineLatencyStage::RenderResourceLockWait,
            Duration::from_micros(300),
            Some(Duration::from_millis(41)),
            Some(FrameMemoryPath::DmaBufZeroCopy),
            queues,
        );

        let snapshot = diagnostics.snapshot_with_queues(queues);
        let render_resource_lock_wait = snapshot.worst_latencies.render_resource_lock_wait;

        assert_eq!(render_resource_lock_wait.samples, 2);
        assert_eq!(
            render_resource_lock_wait.average,
            Duration::from_micros(200)
        );
        assert_eq!(
            render_resource_lock_wait
                .worst
                .map(|sample| sample.duration),
            Some(Duration::from_micros(300))
        );
    }

    #[test]
    fn render_resource_busy_and_reuse_counters_are_aggregated_separately() {
        let mut diagnostics = PlaybackDiagnostics::new();
        let queues = queue_depths_for_tests(0);

        diagnostics.record_render_resource_lock_busy(queues);
        diagnostics.record_render_resource_previous_frame_reuse(queues);
        diagnostics.record_render_resource_previous_frame_reuse(queues);

        let snapshot = diagnostics.snapshot_with_queues(queues);

        assert_eq!(snapshot.render_resource_lock_busy_count, 1);
        assert_eq!(snapshot.render_resource_previous_frame_reuse_count, 2);
    }
}
