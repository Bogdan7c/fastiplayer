//! Итог работы сервера, который отдаёт `shutdown`: потери видны, а не молчаливы.

/// Отчёт о завершении сервера.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ServerShutdownReport {
    /// Кусков байтов, переданных приёмнику.
    pub delivered_chunks: u64,
    /// Двоичных рамок старого поколения, выброшенных сторожем.
    pub dropped_stale_frames: u64,
    /// Текстовых сообщений старого поколения, выброшенных сторожем.
    pub dropped_stale_messages: u64,
    /// Сообщений страницы, потерянных из-за переполнения приёмника событий.
    pub dropped_page_messages: u64,
    /// Потоков, не завершившихся за `shutdown_join_timeout` и отпущенных.
    pub detached_threads: usize,
}
