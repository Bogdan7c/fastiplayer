use rusqlite::{Connection, OptionalExtension, params};

use crate::{StorageError, StorageResult};

/// Последняя известная позиция воспроизведения media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackProgressEntry {
    /// Стабильный идентификатор источника: file path, URL или service URI.
    pub source_uri: String,

    /// Позиция воспроизведения в миллисекундах.
    pub position_ms: i64,

    /// Длительность media в миллисекундах, если она известна.
    pub duration_ms: Option<i64>,

    /// `true`, если media считается досмотренным.
    pub completed: bool,

    /// Unix timestamp обновления в секундах.
    pub updated_at_unix_seconds: i64,
}

/// Сохраняет прогресс воспроизведения через atomic upsert.
pub(crate) fn save_playback_progress(
    connection: &mut Connection,
    playback_progress: &PlaybackProgressEntry,
) -> StorageResult<()> {
    validate_playback_progress(playback_progress)?;

    let transaction = connection
        .transaction()
        .map_err(|source| StorageError::database("begin playback progress transaction", source))?;

    transaction
        .execute(
            r#"
            INSERT INTO playback_progress (
                source_uri,
                position_ms,
                duration_ms,
                completed,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(source_uri) DO UPDATE SET
                position_ms = excluded.position_ms,
                duration_ms = excluded.duration_ms,
                completed = excluded.completed,
                updated_at = excluded.updated_at
            "#,
            params![
                &playback_progress.source_uri,
                playback_progress.position_ms,
                playback_progress.duration_ms,
                playback_progress.completed,
                playback_progress.updated_at_unix_seconds,
            ],
        )
        .map_err(|source| StorageError::database("upsert playback progress", source))?;

    transaction
        .commit()
        .map_err(|source| StorageError::database("commit playback progress transaction", source))
}

/// Загружает прогресс воспроизведения по source URI.
pub(crate) fn load_playback_progress(
    connection: &Connection,
    source_uri: &str,
) -> StorageResult<Option<PlaybackProgressEntry>> {
    validate_source_uri(source_uri)?;

    connection
        .query_row(
            r#"
            SELECT
                source_uri,
                position_ms,
                duration_ms,
                completed,
                updated_at
            FROM playback_progress
            WHERE source_uri = ?1
            "#,
            params![source_uri],
            |row| {
                let completed_flag: bool = row.get(3)?;

                Ok(PlaybackProgressEntry {
                    source_uri: row.get(0)?,
                    position_ms: row.get(1)?,
                    duration_ms: row.get(2)?,
                    completed: completed_flag,
                    updated_at_unix_seconds: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|source| StorageError::database("load playback progress", source))
}

/// Валидирует запись прогресса перед записью в SQLite.
fn validate_playback_progress(playback_progress: &PlaybackProgressEntry) -> StorageResult<()> {
    validate_source_uri(&playback_progress.source_uri)?;

    if playback_progress.position_ms < 0 {
        return Err(StorageError::invalid_value(
            "position_ms",
            "позиция воспроизведения не может быть отрицательной",
        ));
    }

    if let Some(duration_ms) = playback_progress.duration_ms
        && duration_ms < 0
    {
        return Err(StorageError::invalid_value(
            "duration_ms",
            "длительность не может быть отрицательной",
        ));
    }

    if playback_progress.updated_at_unix_seconds < 0 {
        return Err(StorageError::invalid_value(
            "updated_at_unix_seconds",
            "timestamp обновления не может быть отрицательным",
        ));
    }

    Ok(())
}

/// Проверяет общий идентификатор media source.
fn validate_source_uri(source_uri: &str) -> StorageResult<()> {
    if source_uri.trim().is_empty() {
        return Err(StorageError::invalid_value(
            "source_uri",
            "идентификатор media source не может быть пустым",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет сохранение и загрузку прогресса.
    #[test]
    fn playback_progress_round_trip() {
        let mut connection = migrated_connection();
        let playback_progress = PlaybackProgressEntry {
            source_uri: "file:///video.webm".to_string(),
            position_ms: 2_500,
            duration_ms: Some(10_000),
            completed: false,
            updated_at_unix_seconds: 42,
        };

        save_playback_progress(&mut connection, &playback_progress).expect("progress saved");

        let loaded_progress = load_playback_progress(&connection, "file:///video.webm")
            .expect("progress loaded")
            .expect("progress exists");

        assert_eq!(loaded_progress, playback_progress);
    }

    /// Проверяет, что отрицательная позиция не пишется в базу.
    #[test]
    fn negative_position_is_rejected() {
        let mut connection = migrated_connection();
        let playback_progress = PlaybackProgressEntry {
            source_uri: "file:///video.webm".to_string(),
            position_ms: -1,
            duration_ms: None,
            completed: false,
            updated_at_unix_seconds: 1,
        };

        let error = save_playback_progress(&mut connection, &playback_progress)
            .expect_err("invalid progress rejected");

        assert!(error.to_string().contains("position_ms"));
    }

    /// Создаёт in-memory базу с применённой Phase 6 schema.
    fn migrated_connection() -> Connection {
        let mut connection = Connection::open_in_memory().expect("in-memory sqlite opened");
        crate::migrations::migrate(&mut connection).expect("migrations applied");
        connection
    }
}
