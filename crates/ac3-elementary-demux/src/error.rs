//! Typed ошибки demuxer-а «сырого» AC-3 / E-AC-3.

use std::time::Duration;

/// Ошибка открытия, чтения или seek-а elementary stream-а.
///
/// Тексты — английские (уходят в логи и в пользовательские сообщения через
/// anyhow-цепочку); исходная ошибка источника сохраняется как `source`, чтобы
/// player мог отличить пропавшую сеть от битого файла через downcast.
#[derive(Debug, thiserror::Error)]
pub enum Ac3ElementaryDemuxError {
    /// Caller отменил операцию через `CancellationToken`.
    #[error("AC-3 elementary demux was cancelled")]
    Cancelled,
    /// Форма входа, которую сырой поток не поддерживает (сегменты HLS/DASH).
    #[error("AC-3 elementary streams cannot be read from {input} input")]
    UnsupportedInput {
        /// Имя формы входа для диагностики.
        input: &'static str,
    },
    /// Ошибка seekable/byte-source источника.
    #[error("Reading the AC-3 elementary source failed")]
    SourceRead(#[source] source_core::SourceError),
    /// Ошибка последовательного потока.
    #[error("Reading the AC-3 elementary stream failed")]
    StreamRead(#[source] std::io::Error),
    /// В окне открытия нет ни одного целого access unit-а.
    #[error("No AC-3/E-AC-3 access unit was found in the first {scanned_bytes} bytes")]
    NoAccessUnit {
        /// Сколько байт просмотрено.
        scanned_bytes: usize,
    },
    /// ID3v2-тег в начале файла объявляет размер больше предела.
    #[error(
        "The leading ID3v2 tag declares {declared_bytes} bytes, above the limit of {limit_bytes}"
    )]
    LeadingTagTooLarge {
        /// Объявленный размер тега вместе с заголовком.
        declared_bytes: u64,
        /// Действующий предел `max_leading_tag_bytes`.
        limit_bytes: usize,
    },
    /// Синхронизация потеряна и не восстановилась в пределах `max_resync_bytes`.
    #[error(
        "AC-3 sync was lost at byte {resync_start_offset}: no decodable access unit within {discarded_bytes} bytes"
    )]
    SyncLost {
        /// Абсолютное смещение, с которого начался поиск синхронизации.
        resync_start_offset: u64,
        /// Сколько байт отброшено подряд.
        discarded_bytes: usize,
    },
    /// Частота дискретизации не даёт корректной временной базы.
    #[error("The AC-3 sample rate {sample_rate} Hz cannot form a time base")]
    UnrepresentableTimeBase {
        /// Частота из заголовка syncframe-а.
        sample_rate: u32,
    },
    /// Источник не поддерживает переход по байтам.
    #[error("The AC-3 elementary source is not seekable")]
    NotSeekable,
    /// Ни одного целого access unit-а не нашлось ни у цели, ни раньше неё.
    #[error("No AC-3 access unit was found at or before {target:?}")]
    SeekTargetUnavailable {
        /// Запрошенная позиция.
        target: Duration,
    },
    /// Seek не удался, и вернуть прежнюю позицию чтения тоже не удалось.
    #[error("AC-3 seek failed ({original}); restoring the previous read position failed as well")]
    SeekRollbackFailed {
        /// Исходная ошибка seek-а.
        original: Box<Self>,
        /// Ошибка отката.
        #[source]
        rollback: Box<Self>,
    },
}

/// Ошибка построения [`crate::Ac3ElementaryDemuxOptions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Ac3ElementaryOptionsError {
    /// Предел равен нулю.
    #[error("AC-3 elementary demux limit `{name}` must be greater than zero")]
    ZeroLimit {
        /// Имя предела.
        name: &'static str,
    },
}
