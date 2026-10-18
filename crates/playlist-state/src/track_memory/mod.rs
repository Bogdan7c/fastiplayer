//! Память выбора дорожек по видео: аудио, субтитры и задержка субтитров.
//!
//! Нейтральный слой хранения, без знаний о приложении:
//! - [`VideoKey`] — необратимый отпечаток точного локатора (путь/URL в файл не попадает);
//! - [`TrackMemory`] — ограниченная память с вытеснением давних записей (LRU);
//! - [`find_matching_track`] — чистый подбор дорожки по запомненному описанию;
//! - [`TrackMemoryStore`] — инспекция, карантин и атомарная запись `track-memory.json`;
//! - [`TrackMemoryWriter`] — фоновая запись «только последнее».
//!
//! Решения о том, когда читать/писать и что логировать (`tracing::warn!` без путей), остаются
//! у приложения: crate возвращает типизированные исходы.

mod dto;
mod hint;
mod key;
mod memory;
mod store;
mod stream_choice;
mod web_audio_language;
mod web_choice;
mod writer;

#[cfg(test)]
mod tests;

pub use dto::CURRENT_TRACK_MEMORY_SCHEMA_VERSION;
pub use hint::{
    MAX_HINT_CODEC_ID_BYTES, MAX_HINT_LANGUAGE_BYTES, MAX_HINT_TITLE_BYTES, RememberedTrackHint,
    TrackHintCandidate, TrackHintError, TrackHintMatch, find_matching_track,
};
pub use key::{VideoKey, VideoKeyError, VideoSegment};
pub use memory::{
    MAX_REMEMBERED_FILE_PATH_UNITS, MAX_SUBTITLE_SYNC_OFFSET_MILLIS, RecencyStamp, RememberOutcome,
    RememberedFilePath, RememberedFilePathError, RememberedSubtitleChoice, SubtitleSyncOffset,
    SubtitleSyncOffsetOutOfRange, TRACK_MEMORY_HARD_MAX_ENTRIES, TouchOutcome, TrackMemory,
    TrackMemoryCapacity, TrackMemoryCapacityError, TrackMemoryEntry,
};
pub use store::{
    MAX_TRACK_MEMORY_BYTES, TRACK_MEMORY_FILENAME, TrackMemoryCorruptCause, TrackMemoryInspection,
    TrackMemoryStore,
};
pub use stream_choice::{RememberedStreamSubtitle, RememberedStreamSubtitleError};
pub use web_audio_language::{RememberedWebAudioLanguage, RememberedWebAudioLanguageError};
pub use web_choice::{
    RememberedWebSubtitle, RememberedWebSubtitleError, RememberedWebSubtitleKind,
};
pub use writer::{
    TrackMemorySaveRevision, TrackMemorySubmitOutcome, TrackMemoryWriteReport,
    TrackMemoryWriteSnapshot, TrackMemoryWriter, TrackMemoryWriterShutdownOutcome,
    TrackMemoryWriterStartError,
};
