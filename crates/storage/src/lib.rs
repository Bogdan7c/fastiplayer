//! SQLite-хранилище rustiplayer.
//!
//! Crate отвечает за долговременные данные приложения: историю открытия,
//! позицию воспроизведения и cache результатов probing. Пользовательский TOML
//! config намеренно живёт в `rustiplayer-config`, чтобы настройки и runtime
//! данные не смешивались.

#![forbid(unsafe_code)]

mod capability_cache;
mod connection;
mod error;
mod migrations;
mod paths;
mod playback_history;
mod playback_progress;

pub use capability_cache::CapabilityCacheEntry;
pub use connection::{OpenedStorage, StorageConnection, open_or_create, open_or_create_at};
pub use error::{StorageError, StorageResult};
pub use migrations::{
    AppliedMigration, CURRENT_STORAGE_SCHEMA_VERSION, MigrationReport, current_schema_version,
};
pub use paths::{DATABASE_FILE_NAME, StoragePaths};
pub use playback_history::{OpenedMediaRecord, PlaybackHistoryEntry};
pub use playback_progress::PlaybackProgressEntry;
