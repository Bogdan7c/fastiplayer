//! Ошибки разбора и сборки записи. Каждая называет причину, чтобы по ней можно было чинить файл.

use crate::media::MediaError;

/// Ошибка работы с записью страницы.
#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    /// Файл короче заголовка (магия + длина манифеста).
    #[error("recording is too short: {length} bytes, the header needs at least {needed}")]
    TooShort { length: usize, needed: usize },
    /// Первые 8 байт не `FPREC001`.
    #[error("bad magic: expected FPREC001, got {found:?}")]
    BadMagic { found: [u8; 8] },
    /// Длина манифеста выходит за границы файла.
    #[error("manifest length {declared} does not fit: only {available} bytes follow the header")]
    ManifestOutOfFile { declared: usize, available: usize },
    /// Манифест — не JSON.
    #[error("manifest is not valid JSON: {0}")]
    ManifestJson(#[source] serde_json::Error),
    /// В манифесте нет обязательного поля или оно не того типа.
    #[error("manifest field '{field}' is {problem}")]
    ManifestField {
        field: &'static str,
        problem: &'static str,
    },
    /// Не наш формат.
    #[error("unsupported recording format '{found}' (expected '{expected}')")]
    UnsupportedFormat {
        found: String,
        expected: &'static str,
    },
    /// Не та версия формата.
    #[error("unsupported recording version {found} (this reader knows {expected})")]
    UnsupportedVersion { found: u64, expected: u64 },
    /// Событие — не объект JSON.
    #[error("event #{index} is not a JSON object")]
    EventNotObject { index: usize },
    /// У события нет общего обязательного поля (`t_ms`, `ctx`, `op`) или оно не того типа.
    #[error("event #{index}: common field '{field}' is {problem}")]
    EventCommonField {
        index: usize,
        field: &'static str,
        problem: &'static str,
    },
    /// У известной операции нет обязательного поля.
    #[error("event #{index} (op '{op}'): required field '{field}' is missing")]
    MissingField {
        index: usize,
        op: String,
        field: &'static str,
    },
    /// Поле есть, но не того типа (например, `offset` строкой).
    #[error("event #{index} (op '{op}'): field '{field}' must be {expected}")]
    WrongFieldType {
        index: usize,
        op: String,
        field: &'static str,
        expected: &'static str,
    },
    /// Нагрузка `append` выходит за склад байтов.
    #[error(
        "event #{index}: append payload [{offset}, {offset}+{length}) is outside the {store_length}-byte store"
    )]
    PayloadOutOfStore {
        index: usize,
        offset: u64,
        length: u64,
        store_length: usize,
    },
    /// Хеш нагрузки не совпал с записанным — файл повреждён или склад сдвинут.
    #[error("event #{index}: append hash mismatch: recorded {recorded}, computed {computed}")]
    PayloadHashMismatch {
        index: usize,
        recorded: String,
        computed: String,
    },
    /// Манифест при сборке не уместился в u32.
    #[error("manifest is too large to encode: {0} bytes")]
    ManifestTooLarge(usize),
    /// Ассет стенда не подходит для синтетической записи.
    #[error("asset '{asset}' cannot be turned into a recording: {reason}")]
    UnsuitableAsset { asset: String, reason: String },
    /// Ошибка каталога медиа.
    #[error("media: {0}")]
    Media(#[from] MediaError),
    /// Ошибка чтения файла ассета.
    #[error("cannot read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}
