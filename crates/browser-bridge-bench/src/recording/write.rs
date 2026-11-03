//! Сборка файла `*.fprec`: нужна генератору синтетических записей и тестам разбора.
//! Рекордер экстеншена пишет тот же формат на JS; здесь — эталонная реализация на стороне стенда.

use serde_json::{Map, Value, json};

use super::error::RecordingError;
use super::format::{FORMAT_NAME, FORMAT_VERSION, MAGIC, fnv1a_32_hex};

/// Накапливает события и склад байтов, затем выдаёт готовый файл.
pub struct RecordingBuilder {
    browser: String,
    origin: String,
    truncated: bool,
    events: Vec<Value>,
    store: Vec<u8>,
}

impl RecordingBuilder {
    /// Новая пустая запись.
    pub fn new(browser: &str, origin: &str) -> Self {
        Self {
            browser: browser.to_owned(),
            origin: origin.to_owned(),
            truncated: false,
            events: Vec::new(),
            store: Vec::new(),
        }
    }

    /// Помечает запись как усечённую (рекордер упёрся в потолок).
    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    /// Добавляет событие: общие поля плюс `fields` (объект JSON с полями операции).
    pub fn event(&mut self, t_ms: f64, ctx: &str, op: &str, fields: Value) {
        let mut object = Map::new();
        object.insert("t_ms".to_owned(), json!(t_ms));
        object.insert("ctx".to_owned(), json!(ctx));
        object.insert("op".to_owned(), json!(op));
        if let Value::Object(extra) = fields {
            object.extend(extra);
        }
        self.events.push(Value::Object(object));
    }

    /// Добавляет событие `append`: байты уходят в склад, offset/length/hash считаются здесь.
    pub fn append(&mut self, t_ms: f64, ctx: &str, ms: &str, sb: &str, bytes: &[u8]) {
        let offset = self.store.len();
        self.store.extend_from_slice(bytes);
        self.event(
            t_ms,
            ctx,
            "append",
            json!({
                "ms": ms,
                "sb": sb,
                "offset": offset,
                "length": bytes.len(),
                "hash": fnv1a_32_hex(bytes),
            }),
        );
    }

    /// Упорядочивает события по `t_ms` (стабильно: при равенстве порядок добавления сохраняется).
    /// Генератор добавляет события пачками по дорожкам, а в файле порядок обязан быть временным.
    pub fn sort_events_by_time(&mut self) {
        let time_of = |event: &Value| event.get("t_ms").and_then(Value::as_f64).unwrap_or(0.0);
        self.events
            .sort_by(|left, right| time_of(left).total_cmp(&time_of(right)));
    }

    /// Собирает файл: магия, длина манифеста, манифест, склад.
    pub fn finish(self) -> Result<Vec<u8>, RecordingError> {
        let manifest = json!({
            "format": FORMAT_NAME,
            "version": FORMAT_VERSION,
            "browser": self.browser,
            "origin": self.origin,
            "started_at_unix_ms": 0,
            "truncated": self.truncated,
            "events": self.events,
        });
        // `to_vec` для `Value` не падает, но ошибку не глотаем: превращаем в явную.
        let manifest_bytes = serde_json::to_vec(&manifest).map_err(RecordingError::ManifestJson)?;
        let manifest_length = u32::try_from(manifest_bytes.len())
            .map_err(|_| RecordingError::ManifestTooLarge(manifest_bytes.len()))?;
        let mut file = Vec::with_capacity(12 + manifest_bytes.len() + self.store.len());
        file.extend_from_slice(MAGIC);
        file.extend_from_slice(&manifest_length.to_le_bytes());
        file.extend_from_slice(&manifest_bytes);
        file.extend_from_slice(&self.store);
        Ok(file)
    }
}
