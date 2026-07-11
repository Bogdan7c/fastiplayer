use super::*;

use crate::codec_adapter::test_support::{FakeSurfaceReadiness, fake_decoded_frame_handle};

/// Создаёт neutral decoded frame без реального VA resource-а для ownership tests.
fn decoded_frame_for_tests(resource_handle: FrameResourceHandle) -> DecodedFrame {
    DecodedFrame {
        generation: 0,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle,
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

/// Создаёт пустой frame pool для reclaim tests без реального VA display.
fn reclaim_frame_pool_for_tests() -> DmaFramePool {
    DmaFramePool::new(16, 16, 0).expect("test frame pool должен создаваться")
}

/// Создаёт diagnostics capacity для focused reclaim tests без runtime decoder-а.
fn reclaim_capacity_for_tests(max_suppressed_reclaim_frames: usize) -> SuppressedReclaimCapacity {
    SuppressedReclaimCapacity::new(
        max_suppressed_reclaim_frames.max(1),
        1,
        max_suppressed_reclaim_frames,
    )
}

/// Проверяет, что конкретный drop reason ставит handle в reclaim queue.
fn assert_reclaim_enqueue_for_reason(reason: &'static str) {
    let mut suppressed_reclaim_queue = VecDeque::new();
    let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
    let mut frame_pool = reclaim_frame_pool_for_tests();
    let initial_free_frames = frame_pool.num_free();
    let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));

    enqueue_suppressed_frame_for_reclaim_in_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        reclaim_capacity_for_tests(4),
        handle,
        reason,
        PrerollFallbackCandidateMetadata {
            pts: Duration::from_millis(700),
            generation: 31,
        },
    )
    .expect("enqueue suppressed handle должен пройти");

    assert_eq!(
        suppressed_reclaim_queue.len(),
        1,
        "drop path должен поставить handle в reclaim queue"
    );
    assert_eq!(
        frame_pool.num_free(),
        initial_free_frames,
        "enqueue не должен освобождать surface сразу"
    );
    assert!(
        !sync_called.get(),
        "обычный enqueue не должен вызывать blocking sync"
    );
    assert_eq!(suppressed_reclaim_counters.total_enqueued, 1);
    assert_eq!(suppressed_reclaim_counters.total_reclaimed, 0);
}

/// Test-only event type для проверки retry state machine без реальной VA-API.
enum FakeDecoderEvent {
    /// Имитирует `DecoderEvent::FormatChanged`.
    FormatChanged,

    /// Имитирует `DecoderEvent::FrameReady` с исходным PTS.
    FrameReady { pts: Duration },
}

/// Fake decoded frame после publish policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakeReadyFrame {
    /// PTS, который пришёл из fake event-а.
    pts: Duration,

    /// Generation, назначенный event drain policy.
    generation: u64,
}

/// Минимальный fake-драйвер для `CheckEvents -> drain -> retry same packet`.
struct FakeRetryDriver {
    /// Заранее заданные ответы fake `decode()`.
    decode_results: VecDeque<std::result::Result<usize, VaapiAdapterDecodeError>>,

    /// Пакеты событий, которые вернёт каждый fake drain.
    drain_batches: VecDeque<Vec<FakeDecoderEvent>>,

    /// История submit-ов: timestamp и копия bitstream-а.
    submissions: Vec<(u64, Vec<u8>)>,

    /// Очередь fake ready frames, по которой проверяем publish policy.
    ready_frames: VecDeque<FakeReadyFrame>,

    /// PTS кадров, отброшенных discard policy.
    discarded_pts: Vec<Duration>,
}

impl FakeRetryDriver {
    /// Создаёт fake-драйвер с управляемыми decode/drain шагами.
    fn new(
        decode_results: Vec<std::result::Result<usize, VaapiAdapterDecodeError>>,
        drain_batches: Vec<Vec<FakeDecoderEvent>>,
    ) -> Self {
        Self {
            decode_results: VecDeque::from(decode_results),
            drain_batches: VecDeque::from(drain_batches),
            submissions: Vec::new(),
            ready_frames: VecDeque::new(),
            discarded_pts: Vec::new(),
        }
    }
}

impl DecoderRetryDriver for FakeRetryDriver {
    /// Возвращает codec label fake-драйвера для retry diagnostics.
    fn codec_label(&self) -> &'static str {
        "fake"
    }

    /// Записывает submit и возвращает следующий заранее заданный результат.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        _decode_hints: VaapiPacketDecodeHints,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
        self.submissions.push((timestamp_us, packet_data.to_vec()));
        self.decode_results
            .pop_front()
            .expect("fake decode result must be provided")
    }

    /// Обрабатывает один пакет fake events.
    fn drain_events(&mut self, policy: DecoderEventDrainPolicy) -> Result<DecoderDrainReport> {
        let mut report = DecoderDrainReport::default();
        let Some(events) = self.drain_batches.pop_front() else {
            return Ok(report);
        };

        for event in events {
            report.events_count += 1;
            match event {
                FakeDecoderEvent::FormatChanged => {
                    report.format_changed = true;
                }
                FakeDecoderEvent::FrameReady { pts } => match policy {
                    DecoderEventDrainPolicy::Publish { generation } => {
                        self.ready_frames
                            .push_back(FakeReadyFrame { pts, generation });
                    }
                    DecoderEventDrainPolicy::Discard { reason: _ } => {
                        self.discarded_pts.push(pts);
                    }
                },
            }
        }

        Ok(report)
    }
}

/// Создаёт нейтральный floor policy для focused state-machine тестов.
fn preroll_floor_policy(
    generation: u64,
    floor_pts: Duration,
    retain_latest_before_floor: bool,
) -> VideoPrerollOutputFloor {
    VideoPrerollOutputFloor {
        generation,
        floor_pts,
        retain_latest_before_floor,
    }
}

/// Проверяет lifecycle cleanup retained candidate-а и queue перед сменой stream/resources.
fn assert_lifecycle_force_drain_clears_candidate_and_queue(reason: &'static str) {
    let mut suppressed_reclaim_queue = VecDeque::new();
    let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
    let mut frame_pool = reclaim_frame_pool_for_tests();
    let (candidate_handle, candidate_sync_called) =
        fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
    let (queued_handle, queued_sync_called) =
        fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
    let mut candidate = Some(PrerollFallbackCandidate::new(
        candidate_handle,
        Duration::from_millis(10),
        7,
    ));
    suppressed_reclaim_queue.push_back(queued_handle);

    let retained_candidate = candidate
        .take()
        .expect("test starts with retained candidate");
    enqueue_suppressed_frame_for_reclaim_in_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        reclaim_capacity_for_tests(4),
        retained_candidate.handle,
        reason,
        retained_candidate.metadata,
    )
    .expect("candidate enqueue перед lifecycle drain должен пройти");
    force_drain_suppressed_surfaces_from_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        reason,
        reclaim_capacity_for_tests(4),
    )
    .expect("lifecycle force-drain должен очистить suppressed state");

    assert!(candidate.is_none());
    assert!(suppressed_reclaim_queue.is_empty());
    assert!(candidate_sync_called.get());
    assert!(queued_sync_called.get());
}

mod config;
mod contracts;
mod event_drain;
mod preroll;
mod reclaim;
