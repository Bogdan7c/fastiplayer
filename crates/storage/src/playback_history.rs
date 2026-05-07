use rusqlite::{Connection, params};

use crate::{StorageError, StorageResult};

/// Media, которое приложение только что открыло.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedMediaRecord {
    /// Стабильный идентификатор источника: file path, URL или service URI.
    pub source_uri: String,

    /// Отображаемое название, если оно уже известно.
    pub title: Option<String>,

    /// Длительность media в миллисекундах, если demuxer её сообщил.
    pub duration_ms: Option<i64>,

    /// Unix timestamp открытия в секундах.
    pub opened_at_unix_seconds: i64,
}

/// Запись истории открытия media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackHistoryEntry {
    /// Стабильный идентификатор источника: file path, URL или service URI.
    pub source_uri: String,

    /// Последнее известное отображаемое название.
    pub title: Option<String>,

    /// Последняя известная длительность media.
    pub duration_ms: Option<i64>,

    /// Unix timestamp первого открытия.
    pub first_opened_at_unix_seconds: i64,

    /// Unix timestamp последнего открытия.
    pub last_opened_at_unix_seconds: i64,

    /// Количество открытий media.
    pub open_count: i64,
}

/// Записывает открытие media через atomic upsert.
pub(crate) fn record_opened_media(
    connection: &mut Connection,
    opened_media: &OpenedMediaRecord,
) -> StorageResult<()> {
    validate_opened_media(opened_media)?;

    let transaction = connection
        .transaction()
        .map_err(|source| StorageError::database("begin opened media transaction", source))?;

    transaction
        .execute(
            r#"
            INSERT INTO playback_history (
                source_uri,
                title,
                duration_ms,
                first_opened_at,
                last_opened_at,
                open_count,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?4, 1, ?4)
            ON CONFLICT(source_uri) DO UPDATE SET
                title = COALESCE(excluded.title, playback_history.title),
                duration_ms = COALESCE(excluded.duration_ms, playback_history.duration_ms),
                last_opened_at = excluded.last_opened_at,
                open_count = playback_history.open_count + 1,
                updated_at = excluded.updated_at
            "#,
            params![
                &opened_media.source_uri,
                opened_media.title.as_deref(),
                opened_media.duration_ms,
                opened_media.opened_at_unix_seconds,
            ],
        )
        .map_err(|source| StorageError::database("upsert playback history", source))?;

    transaction
        .commit()
        .map_err(|source| StorageError::database("commit opened media transaction", source))
}

/// Возвращает последние записи истории открытия.
pub(crate) fn recent_history(
    connection: &Connection,
    limit: usize,
) -> StorageResult<Vec<PlaybackHistoryEntry>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let query_limit = i64::try_from(limit).map_err(|_| {
        StorageError::invalid_value("limit", "значение не помещается в SQLite INTEGER")
    })?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT
                source_uri,
                title,
                duration_ms,
                first_opened_at,
                last_opened_at,
                open_count
            FROM playback_history
            ORDER BY last_opened_at DESC
            LIMIT ?1
            "#,
        )
        .map_err(|source| StorageError::database("prepare recent playback history", source))?;
    let entries = statement
        .query_map(params![query_limit], |row| {
            Ok(PlaybackHistoryEntry {
                source_uri: row.get(0)?,
                title: row.get(1)?,
                duration_ms: row.get(2)?,
                first_opened_at_unix_seconds: row.get(3)?,
                last_opened_at_unix_seconds: row.get(4)?,
                open_count: row.get(5)?,
            })
        })
        .map_err(|source| StorageError::database("query recent playback history", source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| StorageError::database("read recent playback history rows", source))?;

    Ok(entries)
}

/// Валидирует публичную model перед записью в SQLite.
fn validate_opened_media(opened_media: &OpenedMediaRecord) -> StorageResult<()> {
    if opened_media.source_uri.trim().is_empty() {
        return Err(StorageError::invalid_value(
            "source_uri",
            "идентификатор media source не может быть пустым",
        ));
    }

    if let Some(duration_ms) = opened_media.duration_ms
        && duration_ms < 0
    {
        return Err(StorageError::invalid_value(
            "duration_ms",
            "длительность не может быть отрицательной",
        ));
    }

    if opened_media.opened_at_unix_seconds < 0 {
        return Err(StorageError::invalid_value(
            "opened_at_unix_seconds",
            "timestamp открытия не может быть отрицательным",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет upsert истории и увеличение счётчика открытий.
    #[test]
    fn record_opened_media_upserts_history_entry() {
        let mut connection = migrated_connection();
        let first_open = OpenedMediaRecord {
            source_uri: "file:///video.webm".to_string(),
            title: Some("Video".to_string()),
            duration_ms: Some(1_000),
            opened_at_unix_seconds: 10,
        };
        let second_open = OpenedMediaRecord {
            source_uri: "file:///video.webm".to_string(),
            title: None,
            duration_ms: None,
            opened_at_unix_seconds: 20,
        };

        record_opened_media(&mut connection, &first_open).expect("first history write");
        record_opened_media(&mut connection, &second_open).expect("second history write");

        let entries = recent_history(&connection, 10).expect("history read");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source_uri, "file:///video.webm");
        assert_eq!(entries[0].title.as_deref(), Some("Video"));
        assert_eq!(entries[0].duration_ms, Some(1_000));
        assert_eq!(entries[0].first_opened_at_unix_seconds, 10);
        assert_eq!(entries[0].last_opened_at_unix_seconds, 20);
        assert_eq!(entries[0].open_count, 2);
    }

    /// Проверяет, что пустой source_uri не пишется в базу.
    #[test]
    fn empty_source_uri_is_rejected() {
        let mut connection = migrated_connection();
        let opened_media = OpenedMediaRecord {
            source_uri: "   ".to_string(),
            title: None,
            duration_ms: None,
            opened_at_unix_seconds: 1,
        };

        let error =
            record_opened_media(&mut connection, &opened_media).expect_err("invalid source");

        assert!(error.to_string().contains("source_uri"));
    }

    /// Создаёт in-memory базу с применённой Phase 6 schema.
    fn migrated_connection() -> Connection {
        let mut connection = Connection::open_in_memory().expect("in-memory sqlite opened");
        crate::migrations::migrate(&mut connection).expect("migrations applied");
        connection
    }
}
