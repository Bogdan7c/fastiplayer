//! Диагностика очереди кадров «отправлено в GPU, ещё не отдано» (сессия 09a).
//!
//! Очередь живёт внутри `VaapiVideoDecoder` на decoder thread-е
//! (`decoder::gpu_pending_frames`). Счётчики — общие атомики, как у пула
//! выходных surfaces: пишет только decoder thread, читает кто угодно через
//! `VideoDecodeThread::gpu_pending_frame_stats()`. Это backend-local
//! диагностика, а не часть нейтрального `video-core` контракта.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Снимок счётчиков очереди «в полёте».
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GpuPendingFrameStats {
    /// Сколько кадров попало в очередь.
    pub enqueued: u64,
    /// Доделано после подачи следующего packet-а (основной путь конвейера).
    pub completed_after_next_packet: u64,
    /// Доделано без ожидания: GPU уже закончил.
    pub completed_already_ready: u64,
    /// Доделано из-за ошибки неблокирующего запроса готовности.
    pub completed_after_query_error: u64,
    /// Доделано из-за превышения ёмкости очереди.
    pub completed_on_capacity: u64,
    /// Доделано, когда декодеру нечего было подавать (пауза, конец входа).
    pub completed_on_idle: u64,
    /// Доделано из-за исчерпания пула выходных surfaces.
    pub completed_on_output_backpressure: u64,
    /// Доделано на lifecycle boundary (EOF, смена floor/потока/формата).
    pub completed_on_lifecycle: u64,
    /// Выброшено без публикации (seek flush, кадры текущего вызова на boundary, drop).
    pub discarded: u64,
    /// Сколько кадров сейчас ждут GPU.
    pub current_depth: usize,
    /// Наибольшая глубина очереди.
    pub peak_depth: usize,
}

/// Почему кадр покинул очередь — какой счётчик увеличить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GpuPendingFrameExit {
    /// См. [`GpuPendingFrameStats::completed_after_next_packet`].
    CompletedAfterNextPacket,
    /// См. [`GpuPendingFrameStats::completed_already_ready`].
    CompletedAlreadyReady,
    /// См. [`GpuPendingFrameStats::completed_after_query_error`].
    CompletedAfterQueryError,
    /// См. [`GpuPendingFrameStats::completed_on_capacity`].
    CompletedOnCapacity,
    /// См. [`GpuPendingFrameStats::completed_on_idle`].
    CompletedOnIdle,
    /// См. [`GpuPendingFrameStats::completed_on_output_backpressure`].
    CompletedOnOutputBackpressure,
    /// См. [`GpuPendingFrameStats::completed_on_lifecycle`].
    CompletedOnLifecycle,
    /// См. [`GpuPendingFrameStats::discarded`].
    Discarded,
}

/// Атомарные счётчики очереди: пишет decoder thread, читает кто угодно.
///
/// `Relaxed` достаточно: это диагностика без межпоточного протокола; на самом
/// decoder thread-е значения точные, потому что пишет только он.
#[derive(Debug, Default)]
pub struct GpuPendingFrameCounters {
    enqueued: AtomicU64,
    completed_after_next_packet: AtomicU64,
    completed_already_ready: AtomicU64,
    completed_after_query_error: AtomicU64,
    completed_on_capacity: AtomicU64,
    completed_on_idle: AtomicU64,
    completed_on_output_backpressure: AtomicU64,
    completed_on_lifecycle: AtomicU64,
    discarded: AtomicU64,
    current_depth: AtomicUsize,
    peak_depth: AtomicUsize,
}

impl GpuPendingFrameCounters {
    /// Возвращает снимок счётчиков для диагностики и тестов.
    #[must_use]
    pub fn snapshot(&self) -> GpuPendingFrameStats {
        GpuPendingFrameStats {
            enqueued: self.enqueued.load(Ordering::Relaxed),
            completed_after_next_packet: self.completed_after_next_packet.load(Ordering::Relaxed),
            completed_already_ready: self.completed_already_ready.load(Ordering::Relaxed),
            completed_after_query_error: self.completed_after_query_error.load(Ordering::Relaxed),
            completed_on_capacity: self.completed_on_capacity.load(Ordering::Relaxed),
            completed_on_idle: self.completed_on_idle.load(Ordering::Relaxed),
            completed_on_output_backpressure: self
                .completed_on_output_backpressure
                .load(Ordering::Relaxed),
            completed_on_lifecycle: self.completed_on_lifecycle.load(Ordering::Relaxed),
            discarded: self.discarded.load(Ordering::Relaxed),
            current_depth: self.current_depth.load(Ordering::Relaxed),
            peak_depth: self.peak_depth.load(Ordering::Relaxed),
        }
    }

    /// Учитывает кадр, поставленный в очередь; `depth` — глубина после постановки.
    pub(crate) fn record_enqueued(&self, depth: usize) {
        self.enqueued.fetch_add(1, Ordering::Relaxed);
        self.current_depth.store(depth, Ordering::Relaxed);
        self.peak_depth.fetch_max(depth, Ordering::Relaxed);
    }

    /// Учитывает уход кадров из очереди; `depth` — глубина после ухода.
    pub(crate) fn record_exits(&self, exit: GpuPendingFrameExit, frame_count: usize, depth: usize) {
        let counter = match exit {
            GpuPendingFrameExit::CompletedAfterNextPacket => &self.completed_after_next_packet,
            GpuPendingFrameExit::CompletedAlreadyReady => &self.completed_already_ready,
            GpuPendingFrameExit::CompletedAfterQueryError => &self.completed_after_query_error,
            GpuPendingFrameExit::CompletedOnCapacity => &self.completed_on_capacity,
            GpuPendingFrameExit::CompletedOnIdle => &self.completed_on_idle,
            GpuPendingFrameExit::CompletedOnOutputBackpressure => {
                &self.completed_on_output_backpressure
            }
            GpuPendingFrameExit::CompletedOnLifecycle => &self.completed_on_lifecycle,
            GpuPendingFrameExit::Discarded => &self.discarded,
        };
        counter.fetch_add(frame_count as u64, Ordering::Relaxed);
        self.current_depth.store(depth, Ordering::Relaxed);
    }
}
