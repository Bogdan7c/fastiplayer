//! Статистика освобождения кадров: по какой причине кадр вернулся декодеру.

use std::sync::atomic::{AtomicU64, Ordering};

/// Копируемый снимок счётчиков. Каждое освобождение попадает ровно в один из счётчиков причин.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameReleaseStats {
    /// Освобождено сразу: ни один fence ещё не публиковался, кадр не рисовался.
    pub released_without_fence: u64,
    /// Освобождено сразу в `release_frame`: последний fence уже сработал (быстрый путь без потока).
    pub released_after_fence_signaled_fast: u64,
    /// Освобождено потоком-ожидателем после срабатывания fence.
    pub released_by_waiter: u64,
    /// Освобождено из-за потери контекста (немедленно, GPU уже ничего не дочитает).
    pub released_after_context_loss: u64,
    /// Освобождено из-за отказа ожидания/проверки fence: держать кадр вечно значит
    /// заморозить пул декодера, а GPU при отказе мёртв или в неизвестном состоянии.
    pub fence_wait_failures: u64,
    /// Освобождено сразу, потому что поток-ожидатель не удалось запустить.
    pub waiter_spawn_failures: u64,
    /// Освобождено в `renderer_detaching`: GPU успел пройти fence в пределах отсрочки.
    pub released_at_renderer_detach_after_fence: u64,
    /// Освобождено в `renderer_detaching`: отсрочка вышла, а fence ещё не сработал (GPU мог
    /// ещё читать кадр; при уничтожении рендера это осознанная цена, иначе кадр не вернуть).
    pub released_at_renderer_detach_unfinished: u64,
    /// Освобождено сразу, потому что рендер уже отвязан (GPU больше ничего не читает).
    pub released_after_renderer_detach: u64,
    /// Сколько кадров сейчас стоит в очереди ожидания fence.
    pub pending_now: usize,
}

/// Атомарные счётчики причин (разделяются обёрткой, потоком-ожидателем и тестами).
#[derive(Debug, Default)]
pub(super) struct ReleaseCounters {
    pub(super) released_without_fence: AtomicU64,
    pub(super) released_after_fence_signaled_fast: AtomicU64,
    pub(super) released_by_waiter: AtomicU64,
    pub(super) released_after_context_loss: AtomicU64,
    pub(super) fence_wait_failures: AtomicU64,
    pub(super) waiter_spawn_failures: AtomicU64,
    pub(super) released_at_renderer_detach_after_fence: AtomicU64,
    pub(super) released_at_renderer_detach_unfinished: AtomicU64,
    pub(super) released_after_renderer_detach: AtomicU64,
}

impl ReleaseCounters {
    /// Снимок счётчиков с заданной глубиной очереди.
    pub(super) fn snapshot(&self, pending_now: usize) -> FrameReleaseStats {
        FrameReleaseStats {
            released_without_fence: self.released_without_fence.load(Ordering::Relaxed),
            released_after_fence_signaled_fast: self
                .released_after_fence_signaled_fast
                .load(Ordering::Relaxed),
            released_by_waiter: self.released_by_waiter.load(Ordering::Relaxed),
            released_after_context_loss: self.released_after_context_loss.load(Ordering::Relaxed),
            fence_wait_failures: self.fence_wait_failures.load(Ordering::Relaxed),
            waiter_spawn_failures: self.waiter_spawn_failures.load(Ordering::Relaxed),
            released_at_renderer_detach_after_fence: self
                .released_at_renderer_detach_after_fence
                .load(Ordering::Relaxed),
            released_at_renderer_detach_unfinished: self
                .released_at_renderer_detach_unfinished
                .load(Ordering::Relaxed),
            released_after_renderer_detach: self
                .released_after_renderer_detach
                .load(Ordering::Relaxed),
            pending_now,
        }
    }
}
