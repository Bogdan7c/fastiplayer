use super::VaapiSurfaceLifecycleError;
use super::config::VaapiDecoderRuntimeConfig;
use super::preroll::PrerollFallbackCandidateMetadata;
use crate::codec_adapter::VaapiDecodedFrameHandle;
use crate::frame_pool::DmaFramePool;
use anyhow::Result;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, trace, warn};

/// Накопительные counters backend-local suppressed reclaim queue.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct SuppressedReclaimCounters {
    /// Сколько suppressed/candidate handles было поставлено в reclaim queue.
    pub(super) total_enqueued: usize,

    /// Сколько handles было освобождено через readiness/sync reclaim path.
    pub(super) total_reclaimed: usize,

    /// Сколько раз non-blocking readiness query вернул ошибку.
    pub(super) query_errors: usize,

    /// Сколько blocking `sync()` вызвано forced reclaim path-ом.
    pub(super) forced_sync_count: usize,

    /// Сколько раз queue была полной при попытке enqueue.
    pub(super) ring_full_count: usize,
}

/// Backend-local snapshot ёмкости suppressed reclaim queue для диагностики Session E.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct SuppressedReclaimCapacity {
    /// Сколько output descriptors доступно VA decoder-у.
    pub(super) surface_pool_frames: usize,

    /// Сколько frames может временно удерживать backend ready queue.
    pub(super) ready_queue_frames: usize,

    /// Настроенный bound suppressed reclaim queue после env override/normalization.
    pub(super) max_suppressed_reclaim_frames: usize,
}

impl SuppressedReclaimCapacity {
    /// Собирает snapshot из runtime config без чтения mutable decoder state.
    #[must_use]
    pub(super) fn from_runtime_config(runtime_config: VaapiDecoderRuntimeConfig) -> Self {
        Self::new(
            runtime_config.surface_pool_frames,
            runtime_config.ready_queue_frames,
            runtime_config.max_suppressed_reclaim_frames,
        )
    }

    /// Нормализует capacity поля так же, как decoder hot path нормализует queue bound.
    #[must_use]
    pub(super) fn new(
        surface_pool_frames: usize,
        ready_queue_frames: usize,
        max_suppressed_reclaim_frames: usize,
    ) -> Self {
        let normalized_surface_pool_frames = surface_pool_frames.max(1);
        let normalized_ready_queue_frames = ready_queue_frames.max(1);
        let normalized_reclaim_frames = max_suppressed_reclaim_frames
            .max(1)
            .min(normalized_surface_pool_frames);

        Self {
            surface_pool_frames: normalized_surface_pool_frames,
            ready_queue_frames: normalized_ready_queue_frames,
            max_suppressed_reclaim_frames: normalized_reclaim_frames,
        }
    }

    /// Оценивает свободные места в reclaim queue без тяжёлых VA/resource-pool запросов.
    #[must_use]
    pub(super) fn approximate_available_reclaim_slots(self, current_depth: usize) -> usize {
        self.max_suppressed_reclaim_frames
            .saturating_sub(current_depth)
    }

    /// Оценивает surface headroom, который не отдан suppressed reclaim queue.
    #[must_use]
    pub(super) fn approximate_reserved_surface_headroom_frames(self) -> usize {
        self.surface_pool_frames
            .saturating_sub(self.max_suppressed_reclaim_frames)
    }
}

/// Snapshot одного reclaim pass-а вместе с накопительными counters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReclaimPassReport {
    /// Текущая глубина suppressed reclaim queue после pass-а.
    pub(super) current_depth: usize,

    /// Сколько output descriptors доступно VA decoder-у.
    pub(super) surface_pool_frames: usize,

    /// Сколько frames может временно удерживать backend ready queue.
    pub(super) ready_queue_frames: usize,

    /// Настроенный bound suppressed reclaim queue.
    pub(super) max_suppressed_reclaim_frames: usize,

    /// Приблизительно свободные места в reclaim queue после pass-а.
    pub(super) approximate_available_reclaim_slots: usize,

    /// Приблизительный reserve output descriptors вне reclaim queue.
    pub(super) approximate_reserved_surface_headroom_frames: usize,

    /// Сколько handles было поставлено в очередь за жизнь decoder-а.
    pub(super) total_enqueued: usize,

    /// Сколько handles было освобождено за жизнь decoder-а.
    pub(super) total_reclaimed: usize,

    /// Сколько readiness query errors было за жизнь decoder-а.
    pub(super) query_errors: usize,

    /// Сколько forced `sync()` было за жизнь decoder-а.
    pub(super) forced_sync_count: usize,

    /// Сколько overflow ситуаций было за жизнь decoder-а.
    pub(super) ring_full_count: usize,

    /// Сколько handles освободил конкретный reclaim pass.
    pub(super) reclaimed_this_pass: usize,

    /// Сколько query errors увидел конкретный reclaim pass.
    pub(super) query_errors_this_pass: usize,
}

impl SuppressedReclaimCounters {
    /// Собирает report без раскрытия mutable counters наружу decoder-а.
    pub(super) fn report(
        self,
        current_depth: usize,
        capacity: SuppressedReclaimCapacity,
        reclaimed_this_pass: usize,
        query_errors_this_pass: usize,
    ) -> ReclaimPassReport {
        ReclaimPassReport {
            current_depth,
            surface_pool_frames: capacity.surface_pool_frames,
            ready_queue_frames: capacity.ready_queue_frames,
            max_suppressed_reclaim_frames: capacity.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots: capacity
                .approximate_available_reclaim_slots(current_depth),
            approximate_reserved_surface_headroom_frames: capacity
                .approximate_reserved_surface_headroom_frames(),
            total_enqueued: self.total_enqueued,
            total_reclaimed: self.total_reclaimed,
            query_errors: self.query_errors,
            forced_sync_count: self.forced_sync_count,
            ring_full_count: self.ring_full_count,
            reclaimed_this_pass,
            query_errors_this_pass,
        }
    }
}

/// Проверяет fullness reclaim queue без чтения decoder fields извне.
pub(super) fn is_suppressed_reclaim_queue_full(
    current_depth: usize,
    max_suppressed_reclaim_frames: usize,
) -> bool {
    current_depth >= max_suppressed_reclaim_frames.max(1)
}

/// Возвращает backing frame в pool после того, как decoded handle больше не нужен.
pub(super) fn return_frame_to_pool_from_handle(
    frame_pool: &mut DmaFramePool,
    handle: VaapiDecodedFrameHandle,
) {
    let frame_arc = handle.video_frame();
    drop(handle);

    if let Ok(frame) = Arc::try_unwrap(frame_arc) {
        frame_pool.return_frame(frame);
        trace!("Frame returned to pool");
    } else {
        debug!("Frame still referenced by decoder, cannot return to pool yet");
    }
}

/// Синхронно выбрасывает ready handle из редкого discard/lifecycle path-а.
///
/// Обычный suppressed hot path сюда не заходит: там используется дешёвый
/// `surface_ready()` pass. Этот helper нужен для flush/reconfigure tail events,
/// где очередь reclaim после boundary должна остаться пустой.
pub(super) fn sync_discard_ready_frame(
    frame_pool: &mut DmaFramePool,
    handle: VaapiDecodedFrameHandle,
    reason: &'static str,
) -> Result<Duration> {
    let pts_ms = handle.timestamp() / 1000;
    let sync_start = std::time::Instant::now();

    if let Err(error) = handle.sync() {
        warn!(
            error = %error,
            pts_ms,
            reason,
            "Discarded ready VA surface sync failed"
        );
        return Err(anyhow::Error::new(VaapiSurfaceLifecycleError::new(
            format!("discard ready VA surface sync failed during {reason}: {error}"),
        )));
    }

    let sync_latency = sync_start.elapsed();
    return_frame_to_pool_from_handle(frame_pool, handle);
    debug!(
        pts_ms,
        reason,
        sync_us = sync_latency.as_micros(),
        "Discarded decoder-ready VA surface after sync"
    );
    Ok(sync_latency)
}

/// Неблокирующе освобождает suppressed handles, чьи VA surfaces уже готовы.
pub(super) fn reclaim_ready_suppressed_surfaces_from_queue(
    suppressed_reclaim_queue: &mut VecDeque<VaapiDecodedFrameHandle>,
    suppressed_reclaim_counters: &mut SuppressedReclaimCounters,
    frame_pool: &mut DmaFramePool,
    reclaim_capacity: SuppressedReclaimCapacity,
) -> Result<ReclaimPassReport> {
    let queued_handles_to_scan = suppressed_reclaim_queue.len();
    let mut reclaimed_this_pass = 0;
    let mut query_errors_this_pass = 0;

    for _ in 0..queued_handles_to_scan {
        let Some(handle) = suppressed_reclaim_queue.pop_front() else {
            break;
        };
        let pts_ms = handle.timestamp() / 1000;

        match handle.surface_ready() {
            Ok(true) => {
                return_frame_to_pool_from_handle(frame_pool, handle);
                suppressed_reclaim_counters.total_reclaimed += 1;
                reclaimed_this_pass += 1;
            }
            Ok(false) => {
                suppressed_reclaim_queue.push_back(handle);
            }
            Err(error) => {
                suppressed_reclaim_counters.query_errors += 1;
                query_errors_this_pass += 1;
                let retained_depth = suppressed_reclaim_queue.len() + 1;
                warn!(
                    error = %error,
                    pts_ms,
                    current_depth = retained_depth,
                    max_suppressed_reclaim_frames =
                        reclaim_capacity.max_suppressed_reclaim_frames,
                    approximate_available_reclaim_slots = reclaim_capacity
                        .approximate_available_reclaim_slots(retained_depth),
                    approximate_reserved_surface_headroom_frames = reclaim_capacity
                        .approximate_reserved_surface_headroom_frames(),
                    "Suppressed VA surface readiness query failed; keeping handle queued"
                );
                suppressed_reclaim_queue.push_back(handle);
            }
        }
    }

    let report = suppressed_reclaim_counters.report(
        suppressed_reclaim_queue.len(),
        reclaim_capacity,
        reclaimed_this_pass,
        query_errors_this_pass,
    );
    if queued_handles_to_scan > 0
        || report.reclaimed_this_pass > 0
        || report.query_errors_this_pass > 0
    {
        debug!(
            scanned_this_pass = queued_handles_to_scan,
            current_depth = report.current_depth,
            max_suppressed_reclaim_frames = report.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots = report.approximate_available_reclaim_slots,
            approximate_reserved_surface_headroom_frames =
                report.approximate_reserved_surface_headroom_frames,
            surface_pool_frames = report.surface_pool_frames,
            ready_queue_frames = report.ready_queue_frames,
            total_enqueued = report.total_enqueued,
            total_reclaimed = report.total_reclaimed,
            query_errors = report.query_errors,
            forced_sync_count = report.forced_sync_count,
            ring_full_count = report.ring_full_count,
            reclaimed_this_pass = report.reclaimed_this_pass,
            query_errors_this_pass = report.query_errors_this_pass,
            "Suppressed reclaim pass completed"
        );
    }

    Ok(report)
}

/// Blocking fallback для одного oldest suppressed handle.
pub(super) fn force_reclaim_oldest_suppressed_surface_from_queue(
    suppressed_reclaim_queue: &mut VecDeque<VaapiDecodedFrameHandle>,
    suppressed_reclaim_counters: &mut SuppressedReclaimCounters,
    frame_pool: &mut DmaFramePool,
    reason: &'static str,
    reclaim_capacity: SuppressedReclaimCapacity,
) -> Result<bool> {
    let Some(handle) = suppressed_reclaim_queue.pop_front() else {
        return Ok(false);
    };
    let pts_ms = handle.timestamp() / 1000;
    suppressed_reclaim_counters.forced_sync_count += 1;
    let sync_start = std::time::Instant::now();

    if let Err(error) = handle.sync() {
        suppressed_reclaim_queue.push_front(handle);
        warn!(
            error = %error,
            pts_ms,
            reason,
            current_depth = suppressed_reclaim_queue.len(),
            max_suppressed_reclaim_frames =
                reclaim_capacity.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots = reclaim_capacity
                .approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
            approximate_reserved_surface_headroom_frames = reclaim_capacity
                .approximate_reserved_surface_headroom_frames(),
            "Forced suppressed reclaim sync failed; keeping oldest handle queued"
        );
        return Err(anyhow::Error::new(VaapiSurfaceLifecycleError::new(
            format!("forced suppressed reclaim sync failed during {reason}: {error}"),
        )));
    }

    let sync_latency = sync_start.elapsed();
    return_frame_to_pool_from_handle(frame_pool, handle);
    suppressed_reclaim_counters.total_reclaimed += 1;
    debug!(
        pts_ms,
        reason,
        current_depth = suppressed_reclaim_queue.len(),
        max_suppressed_reclaim_frames = reclaim_capacity.max_suppressed_reclaim_frames,
        approximate_available_reclaim_slots =
            reclaim_capacity.approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
        approximate_reserved_surface_headroom_frames =
            reclaim_capacity.approximate_reserved_surface_headroom_frames(),
        sync_us = sync_latency.as_micros(),
        forced_sync_count = suppressed_reclaim_counters.forced_sync_count,
        "Forced reclaimed oldest suppressed VA surface"
    );
    Ok(true)
}

/// Blocking lifecycle drain для suppressed handles, которые нельзя переносить дальше.
pub(super) fn force_drain_suppressed_surfaces_from_queue(
    suppressed_reclaim_queue: &mut VecDeque<VaapiDecodedFrameHandle>,
    suppressed_reclaim_counters: &mut SuppressedReclaimCounters,
    frame_pool: &mut DmaFramePool,
    reason: &'static str,
    reclaim_capacity: SuppressedReclaimCapacity,
) -> Result<()> {
    let initial_depth = suppressed_reclaim_queue.len();
    if initial_depth > 0 {
        debug!(
            reason,
            initial_depth,
            max_suppressed_reclaim_frames = reclaim_capacity.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots =
                reclaim_capacity.approximate_available_reclaim_slots(initial_depth),
            approximate_reserved_surface_headroom_frames =
                reclaim_capacity.approximate_reserved_surface_headroom_frames(),
            "Force-draining suppressed reclaim queue"
        );
    }

    while force_reclaim_oldest_suppressed_surface_from_queue(
        suppressed_reclaim_queue,
        suppressed_reclaim_counters,
        frame_pool,
        reason,
        reclaim_capacity,
    )? {}

    if initial_depth > 0 {
        debug!(
            reason,
            drained_handles = initial_depth,
            current_depth = suppressed_reclaim_queue.len(),
            max_suppressed_reclaim_frames = reclaim_capacity.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots = reclaim_capacity
                .approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
            approximate_reserved_surface_headroom_frames =
                reclaim_capacity.approximate_reserved_surface_headroom_frames(),
            forced_sync_count = suppressed_reclaim_counters.forced_sync_count,
            total_reclaimed = suppressed_reclaim_counters.total_reclaimed,
            "Force-drained suppressed reclaim queue"
        );
    }

    Ok(())
}

/// Ставит suppressed/candidate handle в bounded reclaim queue.
pub(super) fn enqueue_suppressed_frame_for_reclaim_in_queue(
    suppressed_reclaim_queue: &mut VecDeque<VaapiDecodedFrameHandle>,
    suppressed_reclaim_counters: &mut SuppressedReclaimCounters,
    frame_pool: &mut DmaFramePool,
    reclaim_capacity: SuppressedReclaimCapacity,
    handle: VaapiDecodedFrameHandle,
    reason: &'static str,
    metadata: PrerollFallbackCandidateMetadata,
) -> Result<()> {
    reclaim_ready_suppressed_surfaces_from_queue(
        suppressed_reclaim_queue,
        suppressed_reclaim_counters,
        frame_pool,
        reclaim_capacity,
    )?;

    let normalized_reclaim_bound = reclaim_capacity.max_suppressed_reclaim_frames;
    if suppressed_reclaim_queue.len() >= normalized_reclaim_bound {
        suppressed_reclaim_counters.ring_full_count += 1;
        debug!(
            reason,
            generation = metadata.generation,
            incoming_pts_ms = metadata.pts.as_millis(),
            current_depth = suppressed_reclaim_queue.len(),
            max_suppressed_reclaim_frames = normalized_reclaim_bound,
            approximate_available_reclaim_slots = reclaim_capacity
                .approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
            approximate_reserved_surface_headroom_frames =
                reclaim_capacity.approximate_reserved_surface_headroom_frames(),
            ring_full_count = suppressed_reclaim_counters.ring_full_count,
            "Suppressed reclaim queue full; forcing oldest handle sync"
        );

        if let Err(error) = force_reclaim_oldest_suppressed_surface_from_queue(
            suppressed_reclaim_queue,
            suppressed_reclaim_counters,
            frame_pool,
            reason,
            reclaim_capacity,
        ) {
            suppressed_reclaim_queue.push_back(handle);
            suppressed_reclaim_counters.total_enqueued += 1;
            warn!(
                error = %error,
                reason,
                generation = metadata.generation,
                incoming_pts_ms = metadata.pts.as_millis(),
                current_depth = suppressed_reclaim_queue.len(),
                max_suppressed_reclaim_frames = normalized_reclaim_bound,
                approximate_available_reclaim_slots = reclaim_capacity
                    .approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
                approximate_reserved_surface_headroom_frames = reclaim_capacity
                    .approximate_reserved_surface_headroom_frames(),
                "Kept incoming suppressed handle queued after forced reclaim failure"
            );
            return Err(error);
        }
    }

    suppressed_reclaim_queue.push_back(handle);
    suppressed_reclaim_counters.total_enqueued += 1;
    debug!(
        reason,
        generation = metadata.generation,
        pts_ms = metadata.pts.as_millis(),
        current_depth = suppressed_reclaim_queue.len(),
        max_suppressed_reclaim_frames = normalized_reclaim_bound,
        approximate_available_reclaim_slots =
            reclaim_capacity.approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
        approximate_reserved_surface_headroom_frames =
            reclaim_capacity.approximate_reserved_surface_headroom_frames(),
        surface_pool_frames = reclaim_capacity.surface_pool_frames,
        ready_queue_frames = reclaim_capacity.ready_queue_frames,
        total_enqueued = suppressed_reclaim_counters.total_enqueued,
        total_reclaimed = suppressed_reclaim_counters.total_reclaimed,
        forced_sync_count = suppressed_reclaim_counters.forced_sync_count,
        ring_full_count = suppressed_reclaim_counters.ring_full_count,
        "Queued suppressed VA handle for readiness-based reclaim"
    );
    Ok(())
}

/// Владелец bounded очереди и accounting подавленных VA surfaces.
pub(super) struct SuppressedReclaimState {
    queue: VecDeque<VaapiDecodedFrameHandle>,
    counters: SuppressedReclaimCounters,
    capacity: SuppressedReclaimCapacity,
}
impl SuppressedReclaimState {
    pub(super) fn new(runtime_config: VaapiDecoderRuntimeConfig) -> Self {
        Self {
            queue: VecDeque::with_capacity(runtime_config.max_suppressed_reclaim_frames),
            counters: SuppressedReclaimCounters::default(),
            capacity: SuppressedReclaimCapacity::from_runtime_config(runtime_config),
        }
    }
    pub(super) fn depth(&self) -> usize {
        self.queue.len()
    }
    pub(super) fn is_full(&self) -> bool {
        is_suppressed_reclaim_queue_full(
            self.queue.len(),
            self.capacity.max_suppressed_reclaim_frames,
        )
    }
    pub(super) fn reclaim_ready(
        &mut self,
        frame_pool: &mut DmaFramePool,
    ) -> Result<ReclaimPassReport> {
        reclaim_ready_suppressed_surfaces_from_queue(
            &mut self.queue,
            &mut self.counters,
            frame_pool,
            self.capacity,
        )
    }
    pub(super) fn enqueue(
        &mut self,
        frame_pool: &mut DmaFramePool,
        handle: VaapiDecodedFrameHandle,
        reason: &'static str,
        metadata: PrerollFallbackCandidateMetadata,
    ) -> Result<()> {
        enqueue_suppressed_frame_for_reclaim_in_queue(
            &mut self.queue,
            &mut self.counters,
            frame_pool,
            self.capacity,
            handle,
            reason,
            metadata,
        )
    }
    pub(super) fn force_drain(
        &mut self,
        frame_pool: &mut DmaFramePool,
        reason: &'static str,
    ) -> Result<()> {
        force_drain_suppressed_surfaces_from_queue(
            &mut self.queue,
            &mut self.counters,
            frame_pool,
            reason,
            self.capacity,
        )
    }
}
