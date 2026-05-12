use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::{StorageError, StorageResult};

/// Причина, по которой индекс нельзя сохранять между запусками.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaIndexRuntimeOnlyReason {
    /// HTTP/source descriptor не содержит validators, значит bytes нельзя связать с будущим запуском.
    MissingValidators,
}

/// Итог попытки сохранить media index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaIndexSaveOutcome {
    /// Индекс сохранён в SQLite.
    Persisted {
        /// Количество записанных entries.
        entries: usize,
    },

    /// Индекс оставлен только runtime-данными и не записан в SQLite.
    RuntimeOnly {
        /// Объяснение, почему persistence отключён.
        reason: MediaIndexRuntimeOnlyReason,
    },
}

/// HTTP validators, достаточные для связывания persisted index с конкретной версией bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaIndexValidators {
    /// HTTP entity tag.
    pub etag: Option<String>,

    /// HTTP Last-Modified.
    pub last_modified: Option<String>,
}

impl MediaIndexValidators {
    /// Возвращает `true`, если есть хотя бы один validator.
    #[must_use]
    pub fn has_any(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }
}

/// Identity source-а, к которому привязан persisted index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaIndexSourceIdentity {
    /// Локальный файл проверяется через размер, mtime и partial hash.
    Local {
        /// Стабильный ключ source-а, обычно normalized path/fingerprint.
        source_key: String,

        /// URI или path, который удобно показывать в diagnostics.
        source_uri: String,

        /// Размер файла на момент построения index.
        size_bytes: u64,

        /// `mtime` в наносекундах Unix epoch.
        modified_unix_nanos: i64,

        /// Частичный hash первых/последних bytes, вычисленный source layer-ом.
        partial_hash_hex: String,
    },

    /// HTTP/service source проверяется через service id, format id и validators.
    HttpService {
        /// Стабильный ключ source-а, обычно service + media + format.
        source_key: String,

        /// URI или service label для diagnostics.
        source_uri: String,

        /// Opaque id media внутри service layer.
        service_id: String,

        /// Идентификатор выбранного формата/stream-а.
        format_id: String,

        /// Validators прямого HTTP объекта.
        validators: MediaIndexValidators,
    },
}

impl MediaIndexSourceIdentity {
    /// Возвращает стабильный ключ source-а.
    #[must_use]
    pub fn source_key(&self) -> &str {
        match self {
            Self::Local { source_key, .. } | Self::HttpService { source_key, .. } => source_key,
        }
    }

    /// Возвращает display URI source-а.
    #[must_use]
    pub fn source_uri(&self) -> &str {
        match self {
            Self::Local { source_uri, .. } | Self::HttpService { source_uri, .. } => source_uri,
        }
    }

    /// Возвращает `true`, если identity можно использовать для persisted cache.
    #[must_use]
    pub fn is_persistable(&self) -> bool {
        match self {
            Self::Local {
                partial_hash_hex, ..
            } => !partial_hash_hex.trim().is_empty(),
            Self::HttpService { validators, .. } => validators.has_any(),
        }
    }
}

/// Одна точка keyframe/time index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaIndexEntry {
    /// Track id внутри контейнера.
    pub track_id: u32,

    /// Время packet/sample на media timeline в микросекундах.
    pub time_us: i64,

    /// Byte offset в source-е, если container/demuxer умеет его отдавать.
    pub byte_offset: Option<i64>,

    /// Является ли точка keyframe-ом.
    pub keyframe: bool,
}

/// Полный persisted index для одного media source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedMediaIndex {
    /// Identity source-а, без совпадения которой entries нельзя использовать.
    pub identity: MediaIndexSourceIdentity,

    /// Длительность media, если она была известна при построении index.
    pub duration_ms: Option<i64>,

    /// До какой позиции indexer уже просканировал source.
    pub indexed_until_ms: i64,

    /// `true`, если indexer дошёл до EOF/container end.
    pub complete: bool,

    /// Unix timestamp обновления записи.
    pub updated_at_unix_seconds: i64,

    /// Entries index-а.
    pub entries: Vec<MediaIndexEntry>,
}

/// Сохраняет media index, если source identity достаточно сильная для persistence.
pub(crate) fn save_media_index(
    connection: &mut Connection,
    media_index: &PersistedMediaIndex,
) -> StorageResult<MediaIndexSaveOutcome> {
    validate_media_index(media_index)?;

    if !media_index.identity.is_persistable() {
        return Ok(MediaIndexSaveOutcome::RuntimeOnly {
            reason: MediaIndexRuntimeOnlyReason::MissingValidators,
        });
    }

    let transaction = connection
        .transaction()
        .map_err(|source| StorageError::database("begin media index transaction", source))?;

    upsert_media_index_source(&transaction, media_index)?;
    replace_media_index_entries(&transaction, media_index)?;

    transaction
        .commit()
        .map_err(|source| StorageError::database("commit media index transaction", source))?;

    Ok(MediaIndexSaveOutcome::Persisted {
        entries: media_index.entries.len(),
    })
}

/// Загружает persisted index только при полном совпадении identity.
pub(crate) fn load_media_index(
    connection: &Connection,
    identity: &MediaIndexSourceIdentity,
) -> StorageResult<Option<PersistedMediaIndex>> {
    validate_media_index_identity(identity)?;

    let Some(mut media_index) = load_media_index_source(connection, identity.source_key())? else {
        return Ok(None);
    };

    if &media_index.identity != identity {
        return Ok(None);
    }

    media_index.entries = load_media_index_entries(connection, identity.source_key())?;
    Ok(Some(media_index))
}

/// Удаляет persisted index для source key.
pub(crate) fn delete_media_index(
    connection: &mut Connection,
    source_key: &str,
) -> StorageResult<usize> {
    validate_non_empty("source_key", source_key)?;

    connection
        .execute(
            "DELETE FROM media_index_sources WHERE source_key = ?1",
            params![source_key],
        )
        .map_err(|source| StorageError::database("delete media index", source))
}

/// Сохраняет source row и общую progress metadata.
fn upsert_media_index_source(
    transaction: &Transaction<'_>,
    media_index: &PersistedMediaIndex,
) -> StorageResult<()> {
    let sqlite_identity = SqliteMediaIndexIdentity::from_identity(&media_index.identity)?;

    transaction
        .execute(
            r#"
            INSERT INTO media_index_sources (
                source_key,
                source_uri,
                identity_kind,
                local_size_bytes,
                local_mtime_unix_nanos,
                local_partial_hash_hex,
                service_id,
                format_id,
                validator_etag,
                validator_last_modified,
                duration_ms,
                indexed_until_ms,
                complete,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)
            ON CONFLICT(source_key) DO UPDATE SET
                source_uri = excluded.source_uri,
                identity_kind = excluded.identity_kind,
                local_size_bytes = excluded.local_size_bytes,
                local_mtime_unix_nanos = excluded.local_mtime_unix_nanos,
                local_partial_hash_hex = excluded.local_partial_hash_hex,
                service_id = excluded.service_id,
                format_id = excluded.format_id,
                validator_etag = excluded.validator_etag,
                validator_last_modified = excluded.validator_last_modified,
                duration_ms = excluded.duration_ms,
                indexed_until_ms = excluded.indexed_until_ms,
                complete = excluded.complete,
                updated_at = excluded.updated_at
            "#,
            params![
                sqlite_identity.source_key,
                sqlite_identity.source_uri,
                sqlite_identity.identity_kind,
                sqlite_identity.local_size_bytes,
                sqlite_identity.local_mtime_unix_nanos,
                sqlite_identity.local_partial_hash_hex,
                sqlite_identity.service_id,
                sqlite_identity.format_id,
                sqlite_identity.validator_etag,
                sqlite_identity.validator_last_modified,
                media_index.duration_ms,
                media_index.indexed_until_ms,
                media_index.complete,
                media_index.updated_at_unix_seconds,
            ],
        )
        .map_err(|source| StorageError::database("upsert media index source", source))?;

    Ok(())
}

/// Полностью заменяет entries для source key, чтобы stale offsets не оставались в SQLite.
fn replace_media_index_entries(
    transaction: &Transaction<'_>,
    media_index: &PersistedMediaIndex,
) -> StorageResult<()> {
    transaction
        .execute(
            "DELETE FROM media_index_entries WHERE source_key = ?1",
            params![media_index.identity.source_key()],
        )
        .map_err(|source| StorageError::database("delete stale media index entries", source))?;

    let mut statement = transaction
        .prepare(
            r#"
            INSERT INTO media_index_entries (
                source_key,
                track_id,
                time_us,
                byte_offset,
                keyframe
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .map_err(|source| StorageError::database("prepare media index entry insert", source))?;

    for entry in &media_index.entries {
        statement
            .execute(params![
                media_index.identity.source_key(),
                i64::from(entry.track_id),
                entry.time_us,
                entry.byte_offset,
                entry.keyframe,
            ])
            .map_err(|source| StorageError::database("insert media index entry", source))?;
    }

    Ok(())
}

/// Загружает source row без entries.
fn load_media_index_source(
    connection: &Connection,
    source_key: &str,
) -> StorageResult<Option<PersistedMediaIndex>> {
    connection
        .query_row(
            r#"
            SELECT
                source_key,
                source_uri,
                identity_kind,
                local_size_bytes,
                local_mtime_unix_nanos,
                local_partial_hash_hex,
                service_id,
                format_id,
                validator_etag,
                validator_last_modified,
                duration_ms,
                indexed_until_ms,
                complete,
                updated_at
            FROM media_index_sources
            WHERE source_key = ?1
            "#,
            params![source_key],
            |row| {
                let sqlite_identity = SqliteMediaIndexIdentity {
                    source_key: row.get(0)?,
                    source_uri: row.get(1)?,
                    identity_kind: row.get(2)?,
                    local_size_bytes: row.get(3)?,
                    local_mtime_unix_nanos: row.get(4)?,
                    local_partial_hash_hex: row.get(5)?,
                    service_id: row.get(6)?,
                    format_id: row.get(7)?,
                    validator_etag: row.get(8)?,
                    validator_last_modified: row.get(9)?,
                };

                Ok(PersistedMediaIndex {
                    identity: sqlite_identity.into_identity()?,
                    duration_ms: row.get(10)?,
                    indexed_until_ms: row.get(11)?,
                    complete: row.get(12)?,
                    updated_at_unix_seconds: row.get(13)?,
                    entries: Vec::new(),
                })
            },
        )
        .optional()
        .map_err(|source| StorageError::database("load media index source", source))
}

/// Загружает entries для source key в порядке timeline.
fn load_media_index_entries(
    connection: &Connection,
    source_key: &str,
) -> StorageResult<Vec<MediaIndexEntry>> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT track_id, time_us, byte_offset, keyframe
            FROM media_index_entries
            WHERE source_key = ?1
            ORDER BY track_id ASC, time_us ASC
            "#,
        )
        .map_err(|source| StorageError::database("prepare load media index entries", source))?;

    let entries = statement
        .query_map(params![source_key], |row| {
            let track_id: i64 = row.get(0)?;
            let keyframe: bool = row.get(3)?;

            Ok(MediaIndexEntry {
                track_id: u32::try_from(track_id)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, track_id))?,
                time_us: row.get(1)?,
                byte_offset: row.get(2)?,
                keyframe,
            })
        })
        .map_err(|source| StorageError::database("query media index entries", source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| StorageError::database("read media index entry rows", source))?;

    Ok(entries)
}

/// SQLite-friendly identity row без enum-ветвления в SQL calls.
struct SqliteMediaIndexIdentity {
    /// `source_key`.
    source_key: String,

    /// `source_uri`.
    source_uri: String,

    /// Тип identity: `local` или `http_service`.
    identity_kind: String,

    /// Размер local file.
    local_size_bytes: Option<i64>,

    /// Mtime local file.
    local_mtime_unix_nanos: Option<i64>,

    /// Partial hash local file.
    local_partial_hash_hex: Option<String>,

    /// Service media id.
    service_id: Option<String>,

    /// Service format id.
    format_id: Option<String>,

    /// HTTP ETag.
    validator_etag: Option<String>,

    /// HTTP Last-Modified.
    validator_last_modified: Option<String>,
}

impl SqliteMediaIndexIdentity {
    /// Преобразует публичную identity в SQLite row с checked integer conversion.
    fn from_identity(identity: &MediaIndexSourceIdentity) -> StorageResult<Self> {
        match identity {
            MediaIndexSourceIdentity::Local {
                source_key,
                source_uri,
                size_bytes,
                modified_unix_nanos,
                partial_hash_hex,
            } => Ok(Self {
                source_key: source_key.clone(),
                source_uri: source_uri.clone(),
                identity_kind: "local".to_string(),
                local_size_bytes: Some(i64::try_from(*size_bytes).map_err(|_| {
                    StorageError::invalid_value(
                        "size_bytes",
                        "размер не помещается в SQLite INTEGER",
                    )
                })?),
                local_mtime_unix_nanos: Some(*modified_unix_nanos),
                local_partial_hash_hex: Some(partial_hash_hex.clone()),
                service_id: None,
                format_id: None,
                validator_etag: None,
                validator_last_modified: None,
            }),
            MediaIndexSourceIdentity::HttpService {
                source_key,
                source_uri,
                service_id,
                format_id,
                validators,
            } => Ok(Self {
                source_key: source_key.clone(),
                source_uri: source_uri.clone(),
                identity_kind: "http_service".to_string(),
                local_size_bytes: None,
                local_mtime_unix_nanos: None,
                local_partial_hash_hex: None,
                service_id: Some(service_id.clone()),
                format_id: Some(format_id.clone()),
                validator_etag: validators.etag.clone(),
                validator_last_modified: validators.last_modified.clone(),
            }),
        }
    }

    /// Собирает public identity из SQLite row.
    fn into_identity(self) -> rusqlite::Result<MediaIndexSourceIdentity> {
        match self.identity_kind.as_str() {
            "local" => Ok(MediaIndexSourceIdentity::Local {
                source_key: self.source_key,
                source_uri: self.source_uri,
                size_bytes: u64::try_from(self.local_size_bytes.unwrap_or_default())
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, i64::MIN))?,
                modified_unix_nanos: self.local_mtime_unix_nanos.unwrap_or_default(),
                partial_hash_hex: self.local_partial_hash_hex.unwrap_or_default(),
            }),
            "http_service" => Ok(MediaIndexSourceIdentity::HttpService {
                source_key: self.source_key,
                source_uri: self.source_uri,
                service_id: self.service_id.unwrap_or_default(),
                format_id: self.format_id.unwrap_or_default(),
                validators: MediaIndexValidators {
                    etag: self.validator_etag,
                    last_modified: self.validator_last_modified,
                },
            }),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

/// Проверяет весь index перед записью.
fn validate_media_index(media_index: &PersistedMediaIndex) -> StorageResult<()> {
    validate_media_index_identity(&media_index.identity)?;

    if let Some(duration_ms) = media_index.duration_ms
        && duration_ms < 0
    {
        return Err(StorageError::invalid_value(
            "duration_ms",
            "длительность index не может быть отрицательной",
        ));
    }

    if media_index.indexed_until_ms < 0 {
        return Err(StorageError::invalid_value(
            "indexed_until_ms",
            "progress indexer не может быть отрицательным",
        ));
    }

    if media_index.updated_at_unix_seconds < 0 {
        return Err(StorageError::invalid_value(
            "updated_at_unix_seconds",
            "timestamp обновления не может быть отрицательным",
        ));
    }

    for entry in &media_index.entries {
        validate_media_index_entry(entry)?;
    }

    Ok(())
}

/// Проверяет identity source-а без требования persistability.
fn validate_media_index_identity(identity: &MediaIndexSourceIdentity) -> StorageResult<()> {
    validate_non_empty("source_key", identity.source_key())?;
    validate_non_empty("source_uri", identity.source_uri())?;

    match identity {
        MediaIndexSourceIdentity::Local {
            modified_unix_nanos,
            partial_hash_hex,
            ..
        } => {
            if *modified_unix_nanos < 0 {
                return Err(StorageError::invalid_value(
                    "modified_unix_nanos",
                    "mtime не может быть отрицательным",
                ));
            }
            validate_non_empty("partial_hash_hex", partial_hash_hex)?;
        }
        MediaIndexSourceIdentity::HttpService {
            service_id,
            format_id,
            ..
        } => {
            validate_non_empty("service_id", service_id)?;
            validate_non_empty("format_id", format_id)?;
        }
    }

    Ok(())
}

/// Проверяет одну index entry перед записью.
fn validate_media_index_entry(entry: &MediaIndexEntry) -> StorageResult<()> {
    if entry.time_us < 0 {
        return Err(StorageError::invalid_value(
            "time_us",
            "время index entry не может быть отрицательным",
        ));
    }

    if let Some(byte_offset) = entry.byte_offset
        && byte_offset < 0
    {
        return Err(StorageError::invalid_value(
            "byte_offset",
            "byte offset не может быть отрицательным",
        ));
    }

    Ok(())
}

/// Проверяет непустую строку.
fn validate_non_empty(field: &'static str, value: &str) -> StorageResult<()> {
    if value.trim().is_empty() {
        return Err(StorageError::invalid_value(
            field,
            "значение не может быть пустым",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет round-trip persisted index для local identity.
    #[test]
    fn local_media_index_round_trip() {
        let mut connection = migrated_connection();
        let identity = local_identity("hash-a");
        let media_index = media_index(identity.clone());

        let outcome = save_media_index(&mut connection, &media_index).expect("index saved");
        let loaded = load_media_index(&connection, &identity)
            .expect("index loaded")
            .expect("index exists");

        assert_eq!(outcome, MediaIndexSaveOutcome::Persisted { entries: 2 });
        assert_eq!(loaded, media_index);
    }

    /// Проверяет invalidation local index при изменении partial hash.
    #[test]
    fn local_media_index_invalidates_when_partial_hash_changes() {
        let mut connection = migrated_connection();
        let old_identity = local_identity("hash-a");
        let new_identity = local_identity("hash-b");

        save_media_index(&mut connection, &media_index(old_identity)).expect("index saved");

        let loaded = load_media_index(&connection, &new_identity).expect("index load checked");

        assert!(loaded.is_none());
    }

    /// Проверяет persistence HTTP/service index при наличии validators.
    #[test]
    fn http_media_index_with_validators_round_trips() {
        let mut connection = migrated_connection();
        let identity = MediaIndexSourceIdentity::HttpService {
            source_key: "youtube:abc:303".to_string(),
            source_uri: "https://youtube.example/watch?v=abc".to_string(),
            service_id: "abc".to_string(),
            format_id: "303".to_string(),
            validators: MediaIndexValidators {
                etag: Some("\"etag-1\"".to_string()),
                last_modified: None,
            },
        };
        let media_index = media_index(identity.clone());

        let outcome = save_media_index(&mut connection, &media_index).expect("index saved");
        let loaded = load_media_index(&connection, &identity)
            .expect("index loaded")
            .expect("index exists");

        assert_eq!(outcome, MediaIndexSaveOutcome::Persisted { entries: 2 });
        assert_eq!(loaded.identity, identity);
    }

    /// Проверяет, что HTTP/source без validators не пишется в SQLite.
    #[test]
    fn http_media_index_without_validators_is_runtime_only() {
        let mut connection = migrated_connection();
        let identity = MediaIndexSourceIdentity::HttpService {
            source_key: "youtube:abc:303".to_string(),
            source_uri: "https://youtube.example/watch?v=abc".to_string(),
            service_id: "abc".to_string(),
            format_id: "303".to_string(),
            validators: MediaIndexValidators::default(),
        };
        let media_index = media_index(identity.clone());

        let outcome = save_media_index(&mut connection, &media_index).expect("runtime outcome");
        let loaded = load_media_index(&connection, &identity).expect("index load checked");

        assert_eq!(
            outcome,
            MediaIndexSaveOutcome::RuntimeOnly {
                reason: MediaIndexRuntimeOnlyReason::MissingValidators
            }
        );
        assert!(loaded.is_none());
    }

    /// Проверяет, что delete source удаляет entries через cascade.
    #[test]
    fn delete_media_index_removes_entries() {
        let mut connection = migrated_connection();
        let identity = local_identity("hash-a");
        save_media_index(&mut connection, &media_index(identity.clone())).expect("index saved");

        let deleted_rows =
            delete_media_index(&mut connection, identity.source_key()).expect("index deleted");
        let loaded = load_media_index(&connection, &identity).expect("index load checked");

        assert_eq!(deleted_rows, 1);
        assert!(loaded.is_none());
    }

    /// Создаёт local identity для tests.
    fn local_identity(partial_hash_hex: &str) -> MediaIndexSourceIdentity {
        MediaIndexSourceIdentity::Local {
            source_key: "local:/video.webm".to_string(),
            source_uri: "/video.webm".to_string(),
            size_bytes: 123,
            modified_unix_nanos: 456,
            partial_hash_hex: partial_hash_hex.to_string(),
        }
    }

    /// Создаёт компактный index для tests.
    fn media_index(identity: MediaIndexSourceIdentity) -> PersistedMediaIndex {
        PersistedMediaIndex {
            identity,
            duration_ms: Some(10_000),
            indexed_until_ms: 5_000,
            complete: false,
            updated_at_unix_seconds: 42,
            entries: vec![
                MediaIndexEntry {
                    track_id: 1,
                    time_us: 0,
                    byte_offset: Some(100),
                    keyframe: true,
                },
                MediaIndexEntry {
                    track_id: 1,
                    time_us: 1_000_000,
                    byte_offset: None,
                    keyframe: false,
                },
            ],
        }
    }

    /// Создаёт in-memory базу с актуальной schema.
    fn migrated_connection() -> Connection {
        let mut connection = Connection::open_in_memory().expect("in-memory sqlite opened");
        crate::migrations::migrate(&mut connection).expect("migrations applied");
        connection
    }
}
