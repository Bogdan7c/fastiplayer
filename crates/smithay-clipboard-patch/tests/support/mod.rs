//! Общие помощники функциональных тестов локального патча (Linux, `/proc`).

// Каждый тестовый бинарник использует только часть помощников.
#![allow(dead_code)]

use std::fs;

use sctk::reexports::client::Connection;
use smithay_clipboard::Clipboard;

/// Имя потока в `/proc/<pid>/task/<tid>/comm`: ядро обрезает
/// `smithay-clipboard` до 15 байт.
const WORKER_THREAD_COMM: &str = "smithay-clipboa";

/// Состояние «чужой» очереди: обработчики ей не нужны, `roundtrip` обходится
/// внутренним объектом синхронизации wayland-client.
pub struct ForeignQueueState;

/// Подключается к композитору текущей сессии или сообщает о пропуске теста.
pub fn connect_to_session_compositor() -> Option<Connection> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipped: WAYLAND_DISPLAY is not set, no compositor");
        return None;
    }
    match Connection::connect_to_env() {
        Ok(connection) => Some(connection),
        Err(error) => {
            eprintln!("skipped: cannot connect to the compositor: {error}");
            None
        },
    }
}

/// Создаёт буфер обмена поверх уже открытого соединения — так же, как это
/// делает egui-winit поверх соединения окна winit.
pub fn create_clipboard(connection: &Connection) -> Clipboard {
    let display = connection.backend().display_ptr();
    // SAFETY: `display` — живой `wl_display` соединения `connection`; каждый
    // тест держит соединение дольше, чем возвращённый `Clipboard`.
    unsafe { Clipboard::new(display.cast()) }
}

/// Находит единственный поток буфера обмена в текущем процессе.
pub fn spawned_clipboard_worker() -> u32 {
    let worker_threads: Vec<u32> = fs::read_dir("/proc/self/task")
        .expect("process thread list")
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|thread_id| {
            fs::read_to_string(format!("/proc/self/task/{thread_id}/comm"))
                .is_ok_and(|comm| comm.trim_end() == WORKER_THREAD_COMM)
        })
        .collect();
    assert_eq!(worker_threads.len(), 1, "expected exactly one clipboard worker thread");
    worker_threads[0]
}

/// Число добровольных переключений контекста потока — по одному на каждый
/// уход в сон и пробуждение (`voluntary_ctxt_switches`).
pub fn voluntary_context_switches(thread_id: u32) -> u64 {
    let status = fs::read_to_string(format!("/proc/self/task/{thread_id}/status"))
        .expect("clipboard worker thread status");
    status
        .lines()
        .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))
        .and_then(|value| value.trim().parse().ok())
        .expect("voluntary_ctxt_switches field")
}
