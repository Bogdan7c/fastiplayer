//! Идентификатор прогона страницы: связывает страницу, журнал экстеншена и ожидание в роботе.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize};

use super::error::SiteError;

/// Максимальная длина идентификатора (попадает в URL и ключи таблиц).
const MAX_RUN_ID_LEN: usize = 64;
/// Сколько символов остаётся префиксу в `new_unique`: остальное — уникальный хвост.
const MAX_PREFIX_LEN: usize = 32;

/// Идентификатор прогона: строка `[a-z0-9-]{1,64}`. Другого значения создать нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    /// Проверяет строку и оборачивает её.
    pub fn new(value: &str) -> Result<Self, SiteError> {
        let valid_chars = value
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-'));
        if value.is_empty() || value.len() > MAX_RUN_ID_LEN || !valid_chars {
            return Err(SiteError::InvalidRunId(
                value.chars().take(MAX_RUN_ID_LEN).collect(),
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Новый уникальный идентификатор `<префикс>-<pid>-<время>-<счётчик>`.
    ///
    /// Префикс приводится к допустимому алфавиту (прописные → строчные, остальное → `-`) и
    /// обрезается, поэтому функция не может вернуть ошибку.
    pub fn new_unique(prefix: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let cleaned: String = prefix
            .chars()
            .map(|symbol| {
                let lower = symbol.to_ascii_lowercase();
                if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
                    lower
                } else {
                    '-'
                }
            })
            .take(MAX_PREFIX_LEN)
            .collect();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let prefix_part = if cleaned.is_empty() {
            "run".to_owned()
        } else {
            cleaned
        };
        Self(format!(
            "{prefix_part}-{:x}-{:x}-{counter:x}",
            std::process::id(),
            nanos & 0xffff_ffff_ffff
        ))
    }

    /// Строковое представление (для URL `?run=`).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RunId {
    /// Страница присылает идентификатор строкой — проверяем при разборе JSON.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::new(&text).map_err(serde::de::Error::custom)
    }
}
