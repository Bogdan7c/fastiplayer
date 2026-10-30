//! Перехват журнала `tracing` в тестах браузерного режима: проверить, что в журнал
//! попало, а что — нет (инвариант 7: без данных страницы).

use std::io::Write;
use std::sync::{Arc, Mutex};

/// `Write`, который копит вывод subscriber-а в общий буфер теста.
#[derive(Clone, Default)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("poisoned log buffer"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Выполняет `action` на этом потоке и возвращает её итог и всё, что она записала
/// в журнал (все уровни).
pub(crate) fn with_captured_log<R>(action: impl FnOnce() -> R) -> (R, String) {
    let captured = CapturedLog::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let result = tracing::subscriber::with_default(subscriber, action);
    let text = captured
        .0
        .lock()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    (result, text)
}
