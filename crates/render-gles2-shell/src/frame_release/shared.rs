//! Общее состояние привязки и потока-ожидателя, плюс гарантия «отпустить ровно один раз».

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use video_backend_api::{PresentFrameResourceProviderHandle, VideoBackendLifetimeGuard};
use video_core::FrameResourceHandle;

use super::fence::PresentedFrameFence;
use super::stats::{FrameReleaseStats, ReleaseCounters};

/// Один кадр декодера, который надо вернуть провайдеру ровно один раз.
pub(super) struct PendingRelease {
    /// Внутренний провайдер остаётся владельцем фактического освобождения ресурса.
    inner_provider: PresentFrameResourceProviderHandle,
    /// Непрозрачный handle кадра.
    handle: FrameResourceHandle,
    /// Декодер живёт, пока кадр не освобождён (иначе освобождать было бы некому).
    _backend_lifetime: VideoBackendLifetimeGuard,
    /// Поток-ожидатель, потеря контекста и быстрый путь соревнуются через CAS.
    released: AtomicBool,
}

impl PendingRelease {
    pub(super) fn new(
        inner_provider: PresentFrameResourceProviderHandle,
        handle: FrameResourceHandle,
        backend_lifetime: VideoBackendLifetimeGuard,
    ) -> Self {
        Self {
            inner_provider,
            handle,
            _backend_lifetime: backend_lifetime,
            released: AtomicBool::new(false),
        }
    }

    /// Возвращает кадр провайдеру, только если вызывающий выиграл CAS. `true` — выиграл.
    ///
    /// Счётчик причины увеличивается ДО вызова провайдера: тот, кто увидел освобождение у
    /// провайдера, гарантированно видит и счётчик.
    pub(super) fn release_once(&self, reason_counter: &AtomicU64) -> bool {
        let won = self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if won {
            reason_counter.fetch_add(1, Ordering::Relaxed);
            self.inner_provider.release_frame(self.handle);
        }
        won
    }

    /// Кадр уже кем-то возвращён (поток-ожидатель использует это между слайсами ожидания).
    pub(super) fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }
}

/// Запись очереди ожидания: кадр и fence, после которого его можно вернуть.
#[derive(Clone)]
pub(super) struct WaitingRelease {
    /// Монотонный номер записи: поток-ожидатель по нему убирает ровно свою запись.
    pub(super) id: u64,
    pub(super) release: Arc<PendingRelease>,
    pub(super) fence: Arc<dyn PresentedFrameFence>,
}

/// Режим привязки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BindingMode {
    /// Обычная работа: кадры ждут fence.
    Active,
    /// Контекст потерян: GPU ничего не дочитает, все кадры возвращаются сразу.
    ContextLost,
    /// Рендер штатно отвязался (`renderer_detaching`): он вот-вот уничтожит контекст, новых
    /// кадров не будет, а все кадры возвращаются сразу. Fence в привязке больше не держим.
    RendererDetached,
}

/// Жизненный цикл потока-ожидателя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WaiterLifecycle {
    /// Не запускался: ни разу не понадобился (все кадры шли быстрым путём).
    NotStarted,
    /// Работает.
    Running,
    /// Завершился: привязка закрыта и очередь опустела.
    Exited,
}

/// Состояние под одним мьютексом.
pub(super) struct ReleaseState {
    pub(super) mode: BindingMode,
    /// Fence последнего кадра показа; покрывает всю прежнюю работу GPU контекста.
    pub(super) last_fence: Option<Arc<dyn PresentedFrameFence>>,
    /// FIFO-очередь кадров, ждущих fence. Fence в ней монотонны по времени публикации.
    pub(super) waiting: VecDeque<WaitingRelease>,
    pub(super) next_waiting_id: u64,
    pub(super) waiter: WaiterLifecycle,
    /// Поток-ожидатель сейчас держит клон записи очереди (а с ним и fence). Пока `true`,
    /// fence нельзя считать «отпущенным»: его последний владелец мог бы оказаться в потоке
    /// ожидателя и разрушить `EGLSync` там (см. `detach`).
    pub(super) waiter_holds_entry: bool,
    /// Все клоны привязки уничтожены: новых кадров не будет, поток доделывает очередь и выходит.
    pub(super) closed: bool,
}

/// Общее ядро: состояние, условная переменная и счётчики.
pub(super) struct ReleaseCore {
    state: Mutex<ReleaseState>,
    /// Будит поток-ожидатель (новая запись, закрытие) и наблюдателей его выхода в тестах.
    pub(super) wakeup: Condvar,
    pub(super) counters: ReleaseCounters,
}

impl ReleaseCore {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(ReleaseState {
                mode: BindingMode::Active,
                last_fence: None,
                waiting: VecDeque::new(),
                next_waiting_id: 0,
                waiter: WaiterLifecycle::NotStarted,
                waiter_holds_entry: false,
                closed: false,
            }),
            wakeup: Condvar::new(),
            counters: ReleaseCounters::default(),
        }
    }

    /// Берёт состояние. Отравление мьютекса (паника в критической секции) не блокирует
    /// освобождение кадров: как в wgpu-эталоне, продолжаем с последним состоянием и пишем
    /// ошибку. Fail-closed здесь хуже: замороженный пул декодера останавливает воспроизведение,
    /// а критические секции только дописывают/убирают записи и не оставляют состояние
    /// противоречивым.
    pub(super) fn lock_state(&self) -> MutexGuard<'_, ReleaseState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                log::error!(
                    "состояние освобождения кадров GLES2 отравлено паникой; \
                     продолжаем с последним состоянием, чтобы вернуть кадры декодеру"
                );
                poisoned.into_inner()
            }
        }
    }

    /// Ждёт `wakeup` с той же политикой отравления, что и [`Self::lock_state`].
    pub(super) fn wait_for_wakeup<'a>(
        &self,
        guard: MutexGuard<'a, ReleaseState>,
    ) -> MutexGuard<'a, ReleaseState> {
        match self.wakeup.wait(guard) {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Поток-ожидатель закончил с записью и уже уничтожил её клон: будит ждущих в
    /// [`Self::wait_until_waiter_releases_entry`].
    pub(super) fn mark_waiter_entry_dropped(&self) {
        self.lock_state().waiter_holds_entry = false;
        self.wakeup.notify_all();
    }

    /// Ждёт, пока поток-ожидатель не отпустит запись (а с ней fence), но не дольше `timeout`.
    /// `true` — отпустил (или не держал).
    pub(super) fn wait_until_waiter_releases_entry(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.lock_state();
        while state.waiter_holds_entry {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return false;
            };
            state = match self.wakeup.wait_timeout(state, remaining) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
        true
    }

    /// Снимок статистики.
    pub(super) fn stats(&self) -> FrameReleaseStats {
        let pending_now = self.lock_state().waiting.len();
        self.counters.snapshot(pending_now)
    }
}

#[cfg(test)]
impl ReleaseCore {
    /// Состояние потока-ожидателя (для тестов жизненного цикла).
    pub(super) fn waiter_lifecycle(&self) -> WaiterLifecycle {
        self.lock_state().waiter
    }

    /// Ждёт, пока поток-ожидатель не пометит себя `Exited`; `false` — не дождались за `timeout`.
    pub(super) fn wait_until_waiter_exited(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.lock_state();
        while state.waiter != WaiterLifecycle::Exited {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return false;
            };
            state = match self.wakeup.wait_timeout(state, remaining) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
        true
    }

    /// Отравляет мьютекс состояния паникой в другом потоке.
    pub(super) fn poison_state_for_tests(self: &Arc<Self>) {
        let core = Arc::clone(self);
        let _ = std::thread::spawn(move || {
            let _guard = core.state.lock();
            panic!("намеренная паника для проверки отравления");
        })
        .join();
    }
}
