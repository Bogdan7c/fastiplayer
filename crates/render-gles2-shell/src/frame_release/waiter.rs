//! Поток-ожидатель `gles2-frame-release`: ждёт fence и возвращает кадры декодеру.
//!
//! # Жизненный цикл
//!
//! - Стартует лениво, при первой записи в очередь ожидания: если UI показывает кадры, а GPU
//!   успевает раньше `release_frame` (быстрый путь), поток вообще не создаётся.
//! - Один поток на привязку. Пока очередь пуста и привязка жива, спит на `Condvar` без опроса.
//! - Когда уничтожены все клоны привязки (`closed`) и очередь пуста, поток помечает себя
//!   `Exited` и завершается. Записи в очереди держат `VideoBackendLifetimeGuard`, поэтому
//!   декодер живёт до освобождения последнего кадра даже после уничтожения рендера.
//!
//! # Чего поток НЕ делает
//!
//! Поток не владеет GL/EGL-ресурсами: после `renderer_detaching` он не держит ни одного fence
//! (см. `detach`). Иначе последний `GpuFence` умер бы в отсоединённом потоке и вызвал бы
//! `eglDestroySync` и финальный `eglTerminate` одновременно с завершением процесса: на Mesa
//! (`libgallium`, T410) это давало `corrupted double-linked list` при выходе.
//!
//! # Порядок и сроки
//!
//! Очередь FIFO. Fence в ней монотонны (каждый следующий опубликован позже предыдущего, а GL
//! выполняет команды по порядку), поэтому ждать по очереди корректно. Следствие, зафиксированное
//! тестом: если более поздний fence по какой-то причине сработает раньше ранних, его кадр
//! подождёт ранние. Для настоящего GPU это невозможно; для фальшивых fence — допустимо.
//!
//! Ожидание идёт слайсами [`FENCE_WAIT_SLICE`]: сработавший fence прерывает слайс сразу, а срок
//! нужен лишь чтобы между слайсами заметить, что кадр уже вернули другим путём (потеря
//! контекста), и не сидеть на мёртвом fence.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use gles2_egui::{FenceStatus, FenceWaitError};

use super::shared::{ReleaseCore, WaiterLifecycle, WaitingRelease};

/// Имя потока-ожидателя (видно в `top -H`, отладчике, паник-сообщениях).
pub(super) const WAITER_THREAD_NAME: &str = "gles2-frame-release";

/// Длина одного слайса ожидания fence. Сработавший fence возвращает управление сразу, поэтому
/// значение влияет только на задержку реакции на «кадр уже вернули без нас» (потеря контекста):
/// 50 мс не заметны пользователю и не создают заметной нагрузки (один пробуждающий вызов EGL
/// в 50 мс на простаивающий fence).
const FENCE_WAIT_SLICE: Duration = Duration::from_millis(50);

/// Запускает поток-ожидатель. Вызывается под блокировкой состояния при первой записи очереди.
pub(super) fn spawn_waiter(core: Arc<ReleaseCore>) -> io::Result<()> {
    std::thread::Builder::new()
        .name(WAITER_THREAD_NAME.to_owned())
        .spawn(move || run_waiter(&core))
        // Поток отсоединён намеренно: его выход наблюдается через `WaiterLifecycle::Exited`.
        .map(|_detached_handle| ())
}

/// Чем закончилось ожидание одной записи.
enum WaitOutcome {
    /// GPU прошёл fence.
    FenceSignaled,
    /// Кадр уже вернули другим путём (потеря контекста): делать нечего.
    AlreadyReleased,
    /// Ожидание отказало: GPU мёртв или состояние неизвестно.
    FenceFailed(FenceWaitError),
}

/// Основной цикл потока.
fn run_waiter(core: &ReleaseCore) {
    while let Some(entry) = next_waiting_entry(core) {
        let outcome = wait_for_fence(&entry);
        finish_entry(core, &entry, outcome);
        // Клон записи (а с ним ссылка на fence) уничтожаем ДО сигнала «ничего не держу»:
        // `renderer_detaching` дожидается этого сигнала, чтобы последняя ссылка на fence
        // осталась у рендера и `EGLSync` разрушился в его потоке, а не в этом.
        drop(entry);
        core.mark_waiter_entry_dropped();
    }
}

/// Блокируется до появления записи; `None` — привязка закрыта и очередь пуста, пора выходить.
///
/// Запись остаётся в очереди, пока её ждут: так потеря контекста видит и её тоже.
fn next_waiting_entry(core: &ReleaseCore) -> Option<WaitingRelease> {
    let mut state = core.lock_state();
    loop {
        if let Some(front) = state.waiting.front() {
            let entry = front.clone();
            state.waiter_holds_entry = true;
            return Some(entry);
        }
        if state.closed {
            state.waiter = WaiterLifecycle::Exited;
            core.wakeup.notify_all();
            return None;
        }
        state = core.wait_for_wakeup(state);
    }
}

/// Ждёт fence слайсами, не вися на записи, которую уже вернули.
fn wait_for_fence(entry: &WaitingRelease) -> WaitOutcome {
    loop {
        if entry.release.is_released() {
            return WaitOutcome::AlreadyReleased;
        }
        match entry.fence.wait(FENCE_WAIT_SLICE) {
            Ok(FenceStatus::Signaled) => return WaitOutcome::FenceSignaled,
            Ok(FenceStatus::Pending) => {}
            Err(error) => return WaitOutcome::FenceFailed(error),
        }
    }
}

/// Убирает запись из очереди и возвращает кадр (если его ещё не вернули).
///
/// Запись убирается ДО возврата декодеру: тот, кто увидел возврат, видит и `pending_now` без неё.
/// Если очередь успели очистить потерей контекста, записи там уже нет — тогда не трогаем чужое.
fn finish_entry(core: &ReleaseCore, entry: &WaitingRelease, outcome: WaitOutcome) {
    {
        let mut state = core.lock_state();
        if state
            .waiting
            .front()
            .is_some_and(|front| front.id == entry.id)
        {
            state.waiting.pop_front();
        }
    }
    match outcome {
        WaitOutcome::FenceSignaled => {
            entry
                .release
                .release_once(&core.counters.released_by_waiter);
        }
        WaitOutcome::AlreadyReleased => {}
        WaitOutcome::FenceFailed(error) => {
            // Ошибка ожидания = освобождение: при мёртвом GPU fence не сработает никогда, а
            // удержанный кадр навсегда займёт слот пула декодера и остановит воспроизведение.
            log::warn!(
                "ожидание fence кадра показа GLES2 отказало ({error}); кадр возвращён декодеру"
            );
            entry
                .release
                .release_once(&core.counters.fence_wait_failures);
        }
    }
}
