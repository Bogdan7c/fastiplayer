use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use source_core::{CancellationToken, SourceError};

use crate::buffer::PrefetchBufferState;

/// Частота, с которой foreground wait проверяет внешний cancellation token.
const FOREGROUND_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Единственный активный fetch вместе с диапазоном, который он должен материализовать.
///
/// Range хранится рядом с cancellation token-ом, чтобы foreground seek не мог
/// принять решение по одному из этих состояний, не увидев второе под тем же mutex-ом.
#[derive(Debug, Clone)]
pub(crate) struct ActivePrefetchFetch {
    /// Token отмены только этого blocking чтения inner source-а.
    cancellation: CancellationToken,

    /// Absolute offset первого запрошенного byte-а.
    start_offset: u64,

    /// Exclusive верхняя граница запрошенного диапазона.
    end_offset_exclusive: u64,
}

impl ActivePrefetchFetch {
    /// Создаёт active fetch из exact worker request-а без риска arithmetic overflow.
    #[must_use]
    pub(crate) fn new(start_offset: u64, requested_len: usize) -> Self {
        let requested_len = u64::try_from(requested_len)
            .expect("prefetch requested length должен помещаться в u64");
        Self {
            cancellation: CancellationToken::new(),
            start_offset,
            end_offset_exclusive: start_offset.saturating_add(requested_len),
        }
    }

    /// Возвращает worker-у клон token-а для единственного inner read-а.
    #[must_use]
    pub(crate) fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Проверяет, может ли ещё не отменённый fetch удовлетворить forward seek.
    #[must_use]
    pub(crate) fn can_materialize(&self, offset: u64) -> bool {
        !self.cancellation.is_cancelled()
            && offset >= self.start_offset
            && offset < self.end_offset_exclusive
    }

    /// Отменяет только текущий blocking fetch, не затрагивая lifecycle worker-а.
    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
    }
}

/// Снимок counters, полезный для проверки и runtime diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrefetchDiagnostics {
    /// Сколько bytes worker успешно положил в RAM-буфер.
    pub bytes_prefetched: u64,

    /// Сколько непустых chunks worker успешно положил в RAM-буфер.
    pub chunks_fetched: u64,

    /// Сколько раз foreground read ждал данных от worker-а.
    pub foreground_waits: u64,

    /// Сколько раз foreground seek потребовал сбросить окно и заново читать source.
    pub refetches: u64,

    /// Сколько in-flight fetch-ей worker отбросил из-за foreground seek.
    pub cancelled_fetches: u64,
}

/// Общее состояние prefetch-слоя, защищённое одним mutex-ом.
#[derive(Debug)]
pub(crate) struct PrefetchSharedState {
    /// Sliding RAM window, из которого foreground читает без обращения к сети.
    pub buffer: PrefetchBufferState,

    /// Запрос foreground-а переставить worker на новый absolute offset.
    pub seek_request: Option<u64>,

    /// Выбранный fetch; `None` означает, что foreground сейчас нечего coalesce/cancel-ить.
    pub active_fetch: Option<ActivePrefetchFetch>,

    /// Последняя fatal ошибка worker-а, которую foreground должен увидеть как `read` error.
    pub fatal_error: Option<SourceError>,

    /// Флаг остановки, который выставляет `Drop` перед join worker thread.
    pub shutdown: bool,

    /// Накопленные counters prefetch-слоя.
    pub diagnostics: PrefetchDiagnostics,
}

impl PrefetchSharedState {
    /// Переносит logical cursor в ещё не опубликованную часть active fetch-а.
    ///
    /// Возвращаемое значение описывает exact intent: `true` означает, что seek
    /// уже будет удовлетворён текущим request-ом и reset/cancel не требуется.
    pub(crate) fn stage_forward_seek_into_active_fetch(&mut self, offset: u64) -> bool {
        let active_fetch_will_materialize_offset = self
            .active_fetch
            .as_ref()
            .is_some_and(|active_fetch| active_fetch.can_materialize(offset));
        if !active_fetch_will_materialize_offset {
            return false;
        }

        let buffered_end = self.buffer.buffered_end();
        self.buffer.stage_cursor_ahead(offset);
        tracing::debug!(
            offset,
            buffered_end,
            "media prefetch foreground seek присоединился к active fetch"
        );
        true
    }
}

/// Mutex + Condvar boundary для foreground source и background worker.
#[derive(Debug)]
pub(crate) struct PrefetchShared {
    /// Все mutable поля лежат за одним mutex-ом, чтобы predicates для Condvar были атомарными.
    state: Mutex<PrefetchSharedState>,

    /// Condvar будит worker после consume/seek/shutdown и foreground после append/fatal/EOF.
    condvar: Condvar,
}

impl PrefetchShared {
    /// Создаёт shared state вокруг уже настроенного RAM-буфера.
    #[must_use]
    pub fn new(buffer: PrefetchBufferState) -> Self {
        Self {
            state: Mutex::new(PrefetchSharedState {
                buffer,
                seek_request: None,
                active_fetch: None,
                fatal_error: None,
                shutdown: false,
                diagnostics: PrefetchDiagnostics::default(),
            }),
            condvar: Condvar::new(),
        }
    }

    /// Берёт lock и восстанавливает владение state, если другой поток запаниковал под mutex-ом.
    pub fn lock_state(&self) -> MutexGuard<'_, PrefetchSharedState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned_guard| poisoned_guard.into_inner())
    }

    /// Ждёт обычного worker-события; caller обязан проверять predicate после каждого wakeup.
    pub fn wait_state<'guard>(
        &self,
        guard: MutexGuard<'guard, PrefetchSharedState>,
    ) -> MutexGuard<'guard, PrefetchSharedState> {
        self.condvar
            .wait(guard)
            .unwrap_or_else(|poisoned_guard| poisoned_guard.into_inner())
    }

    /// Ждёт foreground-события короткими шагами, чтобы внешний cancellation token мог прервать wait.
    pub fn wait_state_with_cancellation_poll<'guard>(
        &self,
        guard: MutexGuard<'guard, PrefetchSharedState>,
    ) -> MutexGuard<'guard, PrefetchSharedState> {
        let (guard, _timeout) = self
            .condvar
            .wait_timeout(guard, FOREGROUND_WAIT_POLL_INTERVAL)
            .unwrap_or_else(|poisoned_guard| poisoned_guard.into_inner());

        guard
    }

    /// Будит всех waiters, потому одно событие может менять predicates и worker-а, и foreground-а.
    pub fn notify_all(&self) {
        self.condvar.notify_all();
    }
}
