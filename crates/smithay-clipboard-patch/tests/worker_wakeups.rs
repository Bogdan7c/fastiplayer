//! Функциональная проверка локального патча Fastiplayer: поток буфера обмена
//! не просыпается на событиях чужих очередей общего Wayland-соединения.
//!
//! Тест подключается к настоящему композитору текущей сессии (`WAYLAND_DISPLAY`)
//! и без него пропускается: поддельного Wayland-сервера в зависимостях нет, а
//! суть проверки — поведение libwayland при общем сокете. Окон тест не создаёт.
//!
//! Сценарий повторяет то, что происходит в плеере: «главная» очередь того же
//! соединения получает поток событий (здесь — ответы на `wl_display.sync`,
//! в плеере — frame callback-и и освобождение буферов), а поток буфера обмена
//! должен при этом спать. До патча он просыпался на каждом таком событии.

// Общие помощники живут в отдельном модуле, чтобы второй тест не дублировал их.
mod support;

use std::time::Duration;

use smithay_clipboard::Clipboard;
use support::{
    ForeignQueueState, connect_to_session_compositor, spawned_clipboard_worker,
    voluntary_context_switches,
};

/// Сколько событий получает чужая очередь за время замера.
const FOREIGN_ROUNDTRIPS: u64 = 200;

/// Пауза на начальную настройку потока (его собственные roundtrip-ы и привязка seat).
const WORKER_SETTLE_TIME: Duration = Duration::from_millis(300);

/// Допустимое число пробуждений за замер: таймер разбора очереди срабатывает
/// раз в секунду, а сам замер длится десятки миллисекунд. До патча пробуждений
/// было не меньше, чем событий (≥ 200).
const MAX_EXPECTED_WAKEUPS: u64 = FOREIGN_ROUNDTRIPS / 10;

#[test]
fn clipboard_worker_sleeps_through_events_of_other_queues() {
    // Без композитора проверять нечего: тест честно сообщает о пропуске.
    let Some(connection) = connect_to_session_compositor() else {
        return;
    };

    // Соединение `connection` живёт до конца теста и переживает `clipboard`,
    // который явно уничтожается раньше (требование `Clipboard::new`).
    let clipboard = support::create_clipboard(&connection);
    std::thread::sleep(WORKER_SETTLE_TIME);
    let worker_thread = spawned_clipboard_worker();

    // Чужая очередь того же соединения — как очередь окна в winit.
    let mut foreign_queue = connection.new_event_queue::<ForeignQueueState>();
    let mut foreign_state = ForeignQueueState;

    let switches_before = voluntary_context_switches(worker_thread);
    for _ in 0..FOREIGN_ROUNDTRIPS {
        // Каждый roundtrip — отдельное событие `wl_callback.done` на общем сокете.
        foreign_queue.roundtrip(&mut foreign_state).expect("foreign queue roundtrip");
    }
    let wakeups = voluntary_context_switches(worker_thread) - switches_before;
    eprintln!("clipboard worker wakeups: {wakeups} per {FOREIGN_ROUNDTRIPS} foreign events");

    // Остановка потока по команде Exit тоже должна работать в ленивом режиме.
    drop::<Clipboard>(clipboard);

    assert!(
        wakeups <= MAX_EXPECTED_WAKEUPS,
        "clipboard worker woke up {wakeups} times for {FOREIGN_ROUNDTRIPS} events of another queue"
    );
}
