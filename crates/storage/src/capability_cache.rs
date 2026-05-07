use rusqlite::{Connection, OptionalExtension, params};

use crate::{StorageError, StorageResult};

/// Cache entry с результатом capability probing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityCacheEntry {
    /// Стабильный ключ probe, например backend + device id + driver version.
    pub cache_key: String,

    /// Имя backend, который создал cache entry.
    pub backend: String,

    /// JSON payload будущего capability report.
    pub payload_json: String,

    /// Версия schema JSON payload, независимая от SQLite schema.
    pub schema_version: i64,

    /// Unix timestamp probing в секундах.
    pub probed_at_unix_seconds: i64,

    /// Unix timestamp истечения cache entry; `None` означает бессрочную запись.
    pub expires_at_unix_seconds: Option<i64>,
}

/// Сохраняет capability cache entry через atomic upsert.
pub(crate) fn save_capability_cache_entry(
    connection: &mut Connection,
    capability_cache_entry: &CapabilityCacheEntry,
) -> StorageResult<()> {
    validate_capability_cache_entry(capability_cache_entry)?;

    let transaction = connection
        .transaction()
        .map_err(|source| StorageError::database("begin capability cache transaction", source))?;

    transaction
        .execute(
            r#"
            INSERT INTO capability_cache (
                cache_key,
                backend,
                payload_json,
                schema_version,
                probed_at,
                expires_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(cache_key) DO UPDATE SET
                backend = excluded.backend,
                payload_json = excluded.payload_json,
                schema_version = excluded.schema_version,
                probed_at = excluded.probed_at,
                expires_at = excluded.expires_at
            "#,
            params![
                &capability_cache_entry.cache_key,
                &capability_cache_entry.backend,
                &capability_cache_entry.payload_json,
                capability_cache_entry.schema_version,
                capability_cache_entry.probed_at_unix_seconds,
                capability_cache_entry.expires_at_unix_seconds,
            ],
        )
        .map_err(|source| StorageError::database("upsert capability cache", source))?;

    transaction
        .commit()
        .map_err(|source| StorageError::database("commit capability cache transaction", source))
}

/// Загружает capability cache entry по ключу.
pub(crate) fn load_capability_cache_entry(
    connection: &Connection,
    cache_key: &str,
) -> StorageResult<Option<CapabilityCacheEntry>> {
    validate_non_empty("cache_key", cache_key)?;

    connection
        .query_row(
            r#"
            SELECT
                cache_key,
                backend,
                payload_json,
                schema_version,
                probed_at,
                expires_at
            FROM capability_cache
            WHERE cache_key = ?1
            "#,
            params![cache_key],
            |row| {
                Ok(CapabilityCacheEntry {
                    cache_key: row.get(0)?,
                    backend: row.get(1)?,
                    payload_json: row.get(2)?,
                    schema_version: row.get(3)?,
                    probed_at_unix_seconds: row.get(4)?,
                    expires_at_unix_seconds: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|source| StorageError::database("load capability cache entry", source))
}

/// Удаляет cache entries, чей срок действия уже истёк.
pub(crate) fn delete_expired_capability_cache(
    connection: &mut Connection,
    current_unix_seconds: i64,
) -> StorageResult<usize> {
    if current_unix_seconds < 0 {
        return Err(StorageError::invalid_value(
            "current_unix_seconds",
            "текущий timestamp не может быть отрицательным",
        ));
    }

    let deleted_rows = connection
        .execute(
            r#"
            DELETE FROM capability_cache
            WHERE expires_at IS NOT NULL
              AND expires_at <= ?1
            "#,
            params![current_unix_seconds],
        )
        .map_err(|source| StorageError::database("delete expired capability cache", source))?;

    Ok(deleted_rows)
}

/// Валидирует capability cache entry перед записью.
fn validate_capability_cache_entry(entry: &CapabilityCacheEntry) -> StorageResult<()> {
    validate_non_empty("cache_key", &entry.cache_key)?;
    validate_non_empty("backend", &entry.backend)?;
    validate_non_empty("payload_json", &entry.payload_json)?;

    if entry.schema_version <= 0 {
        return Err(StorageError::invalid_value(
            "schema_version",
            "версия payload schema должна быть положительной",
        ));
    }

    if entry.probed_at_unix_seconds < 0 {
        return Err(StorageError::invalid_value(
            "probed_at_unix_seconds",
            "timestamp probing не может быть отрицательным",
        ));
    }

    if let Some(expires_at_unix_seconds) = entry.expires_at_unix_seconds
        && expires_at_unix_seconds < 0
    {
        return Err(StorageError::invalid_value(
            "expires_at_unix_seconds",
            "timestamp истечения cache не может быть отрицательным",
        ));
    }

    Ok(())
}

/// Проверяет, что строковое поле не пустое после trim.
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

    /// Проверяет сохранение и загрузку capability cache entry.
    #[test]
    fn capability_cache_round_trip() {
        let mut connection = migrated_connection();
        let capability_cache_entry = CapabilityCacheEntry {
            cache_key: "vaapi:intel:driver".to_string(),
            backend: "vaapi".to_string(),
            payload_json: r#"{"vp9":true}"#.to_string(),
            schema_version: 1,
            probed_at_unix_seconds: 10,
            expires_at_unix_seconds: Some(20),
        };

        save_capability_cache_entry(&mut connection, &capability_cache_entry)
            .expect("capability cache saved");

        let loaded_entry = load_capability_cache_entry(&connection, "vaapi:intel:driver")
            .expect("capability cache loaded")
            .expect("capability cache exists");

        assert_eq!(loaded_entry, capability_cache_entry);
    }

    /// Проверяет удаление устаревших capability cache entries.
    #[test]
    fn expired_capability_cache_is_deleted() {
        let mut connection = migrated_connection();
        let expired_entry = CapabilityCacheEntry {
            cache_key: "expired".to_string(),
            backend: "vaapi".to_string(),
            payload_json: "{}".to_string(),
            schema_version: 1,
            probed_at_unix_seconds: 1,
            expires_at_unix_seconds: Some(2),
        };
        let fresh_entry = CapabilityCacheEntry {
            cache_key: "fresh".to_string(),
            backend: "vaapi".to_string(),
            payload_json: "{}".to_string(),
            schema_version: 1,
            probed_at_unix_seconds: 1,
            expires_at_unix_seconds: Some(5),
        };

        save_capability_cache_entry(&mut connection, &expired_entry).expect("expired saved");
        save_capability_cache_entry(&mut connection, &fresh_entry).expect("fresh saved");

        let deleted_rows =
            delete_expired_capability_cache(&mut connection, 3).expect("expired deleted");

        assert_eq!(deleted_rows, 1);
        assert!(
            load_capability_cache_entry(&connection, "expired")
                .expect("expired loaded")
                .is_none()
        );
        assert!(
            load_capability_cache_entry(&connection, "fresh")
                .expect("fresh loaded")
                .is_some()
        );
    }

    /// Создаёт in-memory базу с применённой Phase 6 schema.
    fn migrated_connection() -> Connection {
        let mut connection = Connection::open_in_memory().expect("in-memory sqlite opened");
        crate::migrations::migrate(&mut connection).expect("migrations applied");
        connection
    }
}
