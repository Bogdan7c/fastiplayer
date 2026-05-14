use std::collections::VecDeque;
use std::time::Duration;

use video_core::{DecodedFrame, FrameMemoryPath};

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

/// Queue depths, снятые около latency/drop события.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineQueueDepthSnapshot {
    /// Audio packets перед decoder/audio output.
    pub pending_audio_packets: usize,

    /// Video packets перед decoder thread.
    pub pending_video_packets: usize,

    /// Player presentation queue depth.
    pub present_queue_depth: usize,

    /// Decoder send queue depth около отправки packet-а.
    pub decoder_send_queue_depth: usize,

    /// Backend ready queue depth, если decoder сообщил его с frame-ом.
    pub decoder_ready_queue_depth: Option<usize>,

    /// Активные render leases.
    pub active_render_leases: usize,

    /// Texture releases, ожидающие drop render lease-а.
    pub deferred_render_releases: usize,

    /// Texture/surface slot pressure.
    pub texture_slots: Option<TextureSlotPressureSnapshot>,
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

    /// GPU submit/present timing, если доступен.
    pub gpu_submit_present: LatencyCounterSnapshot,

    /// Render release acknowledgement latency.
    pub release_acknowledgement: LatencyCounterSnapshot,
}

/// Drop counters по typed причинам.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoDropCountersSnapshot {
    /// Все typed drops.
    pub total: u64,

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

/// Read-only diagnostics snapshot, который UI может только отображать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackDiagnosticsSnapshot {
    /// Последний observed video memory path.
    pub zero_copy_memory_path: Option<FrameMemoryPath>,

    /// Последние queue depths.
    pub queues: PipelineQueueDepthSnapshot,

    /// Typed drop counters.
    pub drops: VideoDropCountersSnapshot,

    /// Typed pipeline pause counters.
    pub pauses: PipelinePauseCountersSnapshot,

    /// Worst/average latency counters по stage.
    pub worst_latencies: PipelineLatencyCountersSnapshot,

    /// Bounded recent samples, которые обновляли worst latency.
    pub recent_worst_samples: Vec<PipelineLatencySampleSnapshot>,

    /// Количество decoded frames, прошедших через player diagnostics.
    pub decoded_frames: u64,
}

impl Default for PlaybackDiagnosticsSnapshot {
    /// Возвращает пустой snapshot без heap churn в hot path.
    fn default() -> Self {
        Self {
            zero_copy_memory_path: None,
            queues: PipelineQueueDepthSnapshot::default(),
            drops: VideoDropCountersSnapshot::default(),
            pauses: PipelinePauseCountersSnapshot::default(),
            worst_latencies: PipelineLatencyCountersSnapshot::default(),
            recent_worst_samples: Vec::new(),
            decoded_frames: 0,
        }
    }
}

/// Компактная log-сводка без строковых аллокаций на hot path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaybackDiagnosticsLogSummary {
    /// Все typed drops.
    pub drops_total: u64,

    /// Все typed pauses.
    pub pauses_total: u64,

    /// Последний observed memory path.
    pub zero_copy_memory_path: Option<FrameMemoryPath>,

    /// Самая медленная stage на момент summary.
    pub worst_stage: Option<PipelineLatencyStage>,

    /// Worst latency самой медленной stage.
    pub worst_latency: Option<Duration>,

    /// Последние queue depths.
    pub queues: PipelineQueueDepthSnapshot,
}

impl PlaybackDiagnosticsLogSummary {
    /// Возвращает `true`, если summary содержит полезную runtime активность.
    #[must_use]
    pub const fn has_activity(self) -> bool {
        self.drops_total > 0
            || self.pauses_total > 0
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
}

impl PlaybackDiagnostics {
    /// Создаёт пустой bounded aggregator.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            snapshot: PlaybackDiagnosticsSnapshot::default(),
            recent_worst_samples: VecDeque::with_capacity(RECENT_WORST_SAMPLE_LIMIT),
            latency_counters: PipelineLatencyCounters::default(),
        }
    }

    /// Сбрасывает media-specific diagnostics.
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    /// Записывает decoded frame и его backend-provided timings.
    pub(crate) fn observe_decoded_frame(
        &mut self,
        frame: &DecodedFrame,
        queues: PipelineQueueDepthSnapshot,
    ) {
        self.snapshot.decoded_frames = self.snapshot.decoded_frames.saturating_add(1);
        self.snapshot.zero_copy_memory_path = Some(frame.memory_path);
        self.snapshot.queues = queues;

        self.record_optional_latency(
            PipelineLatencyStage::DecoderPacketReceive,
            frame.diagnostics.timings.decoder_packet_receive_latency,
            Some(frame.pts),
            Some(frame.memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::DecoderSubmit,
            frame.diagnostics.timings.decoder_submit_latency,
            Some(frame.pts),
            Some(frame.memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::DecoderEventDrain,
            frame.diagnostics.timings.decoder_event_drain_latency,
            Some(frame.pts),
            Some(frame.memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::HardwareSync,
            frame.diagnostics.timings.hardware_sync_latency,
            Some(frame.pts),
            Some(frame.memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::DmaBufExport,
            frame.diagnostics.timings.dma_buf_export_latency,
            Some(frame.pts),
            Some(frame.memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::DmaBufImport,
            frame.diagnostics.timings.dma_buf_import_latency,
            Some(frame.pts),
            Some(frame.memory_path),
            queues,
        );
        self.record_optional_latency(
            PipelineLatencyStage::DecodedFramePublish,
            frame.diagnostics.timings.decoded_frame_publish_latency,
            Some(frame.pts),
            Some(frame.memory_path),
            queues,
        );
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
                self.snapshot.drops.late = self.snapshot.drops.late.saturating_add(1);
            }
            VideoDropReason::QueueOverflow => {
                self.snapshot.drops.queue_overflow =
                    self.snapshot.drops.queue_overflow.saturating_add(1);
            }
            VideoDropReason::StaleGeneration => {
                self.snapshot.drops.stale_generation =
                    self.snapshot.drops.stale_generation.saturating_add(1);
            }
            VideoDropReason::SeekPreroll => {
                self.snapshot.drops.seek_preroll =
                    self.snapshot.drops.seek_preroll.saturating_add(1);
            }
            VideoDropReason::RenderAcquisitionTimeout => {
                self.snapshot.drops.render_acquisition_timeout = self
                    .snapshot
                    .drops
                    .render_acquisition_timeout
                    .saturating_add(1);
            }
            VideoDropReason::DecoderStarvation => {
                self.snapshot.drops.decoder_starvation =
                    self.snapshot.drops.decoder_starvation.saturating_add(1);
            }
            VideoDropReason::Paused => {
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

    /// Возвращает snapshot с актуальными queue depths.
    #[must_use]
    pub(crate) fn snapshot_with_queues(
        &self,
        queues: PipelineQueueDepthSnapshot,
    ) -> PlaybackDiagnosticsSnapshot {
        let mut snapshot = self.snapshot.clone();
        snapshot.queues = queues;
        snapshot.recent_worst_samples = self.recent_worst_samples.iter().copied().collect();
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
            pauses_total: self.snapshot.pauses.total,
            zero_copy_memory_path: self.snapshot.zero_copy_memory_path,
            worst_stage,
            worst_latency,
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
            PipelineLatencyStage::GpuSubmitPresent => &mut self.gpu_submit_present,
            PipelineLatencyStage::ReleaseAcknowledgement => &mut self.release_acknowledgement,
        }
    }

    /// Возвращает fixed list stage counters для summary.
    fn stage_counters(&self) -> [(PipelineLatencyStage, &LatencyCounter); 12] {
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
    use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
    use video_core::{DecodedPixelFormat, FrameTextureHandle, VideoFrameTimingDiagnostics};

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
            pts: Duration::from_millis(42),
            format: DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: FrameMemoryPath::DmaBufZeroCopy,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: FrameTextureHandle(7),
            diagnostics: video_core::VideoFrameDiagnostics {
                timings: VideoFrameTimingDiagnostics {
                    decoder_submit_latency: Some(Duration::from_millis(3)),
                    dma_buf_import_latency: Some(Duration::from_millis(5)),
                    ..VideoFrameTimingDiagnostics::default()
                },
                decoder_ready_queue_depth: Some(1),
                texture_pool: None,
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
            VideoDropReason::RenderAcquisitionTimeout,
            queues,
        );

        let snapshot = diagnostics.snapshot_with_queues(queues);

        assert_eq!(snapshot.drops.total, 2);
        assert_eq!(snapshot.drops.seek_preroll, 1);
        assert_eq!(snapshot.drops.render_acquisition_timeout, 1);
        assert_eq!(
            snapshot.drops.last.map(|drop| drop.reason),
            Some(VideoDropReason::RenderAcquisitionTimeout)
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
}
