//! Функциональная проверка локального патча Fastiplayer: команды буфера обмена
//! обрабатываются, даже когда поток не слушает общий Wayland-сокет.
//!
//! До первого копирования из приложения поток спит и разбирает свою очередь
//! только по команде (`load`/`store`) или по редкому таймеру. Тест проверяет,
//! что ответ на `load` приходит (а не повисает), а `Drop` завершает поток.
//! Окна у тестового клиента нет, поэтому фокуса клавиатуры нет и `load`
//! обязан вернуть ошибку, а не текст — как и в upstream-версии.
//!
//! Нужен настоящий композитор (`WAYLAND_DISPLAY`), без него тест пропускается.

mod support;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use support::{connect_to_session_compositor, create_clipboard};

/// Предел ожидания ответа: ответ требует одного roundtrip-а с композитором
/// (доли миллисекунды), секунды хватает с огромным запасом.
const REPLY_DEADLINE: Duration = Duration::from_secs(5);

#[test]
fn load_and_shutdown_are_answered_without_socket_watch() {
    let Some(connection) = connect_to_session_compositor() else {
        return;
    };
    let clipboard = create_clipboard(&connection);

    // Команда идёт из отдельного потока: если рабочий поток не ответит,
    // тест упадёт по таймауту, а не зависнет навсегда.
    let (outcome_sender, outcome_receiver) = mpsc::channel();
    let caller = std::thread::spawn(move || {
        // `store` без фокуса ничего не публикует, но не должен ломать очередь команд.
        clipboard.store("fastiplayer clipboard patch test");
        let load_result = clipboard.load();
        let shutdown_started = Instant::now();
        drop(clipboard);
        let _ = outcome_sender.send((load_result, shutdown_started.elapsed()));
    });

    let (load_result, shutdown_time) =
        outcome_receiver.recv_timeout(REPLY_DEADLINE).expect("clipboard worker reply");
    caller.join().expect("caller thread");

    // Без окна фокуса нет: upstream-семантика — ошибка, а не пустая строка.
    let load_error = load_result.expect_err("no keyboard focus means no clipboard text");
    assert!(
        load_error.to_string().contains("no events received on any seat")
            || load_error.to_string().contains("doesn't have focus"),
        "unexpected load error: {load_error}"
    );
    assert!(shutdown_time < REPLY_DEADLINE, "clipboard worker shutdown took {shutdown_time:?}");
    // Соединение уничтожается последним — после потока буфера обмена.
    drop(connection);
}
