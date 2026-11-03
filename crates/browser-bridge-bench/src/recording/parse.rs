//! Разбор файла `*.fprec` в [`Recording`].
//!
//! Правила (контракт формата):
//! - магия, длина манифеста, версия и `format` проверяются строго — иначе это не наша запись;
//! - у известной операции каждое обязательное поле обязано быть, иначе ошибка с номером
//!   события и именем поля;
//! - неизвестная операция и неизвестные поля пропускаются с предупреждением
//!   ([`Recording::warnings`]): запись, сделанная более новым рекордером, остаётся читаемой;
//! - нагрузка `append` обязана лежать внутри склада, а её FNV-1a — совпадать с записанным.

use std::ops::Range;

use serde_json::{Map, Value};

use super::error::RecordingError;
use super::format::{
    COMMON_EVENT_FIELDS, FORMAT_NAME, FORMAT_VERSION, HEADER_LENGTH, KNOWN_MANIFEST_FIELDS, MAGIC,
    MAIN_CONTEXT, OpSpec, fnv1a_32_hex, op_spec,
};

/// Поля манифеста, нужные стенду (остальное хранится в событиях).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingManifest {
    /// `firefox` или `chrome` — в каком браузере записано.
    pub browser: String,
    /// Origin страницы-источника (без пути и запроса).
    pub origin: String,
    /// Момент старта записи, мс Unix.
    pub started_at_unix_ms: u64,
    /// `true` — рекордер упёрся в потолок, хвост потока не записан.
    pub truncated: bool,
}

/// Одно событие записи, прошедшее проверку.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedEvent {
    /// Порядковый номер в манифесте (для сообщений об ошибках).
    pub index: usize,
    /// Миллисекунды от старта записи по часам главного потока.
    pub t_ms: f64,
    /// `main` или `worker-<N>`.
    pub ctx: String,
    /// Имя операции (всегда известная: неизвестные отброшены при разборе).
    pub op: String,
    /// Все поля события, включая общие.
    pub fields: Map<String, Value>,
    /// Диапазон нагрузки в складе байтов — только у `append`.
    pub payload: Option<Range<usize>>,
}

impl RecordedEvent {
    /// Строковое поле события.
    pub fn text(&self, field: &str) -> Option<&str> {
        self.fields.get(field).and_then(Value::as_str)
    }
}

/// Разобранная запись: манифест, события, склад байтов и предупреждения разбора.
#[derive(Debug, Clone)]
pub struct Recording {
    pub manifest: RecordingManifest,
    events: Vec<RecordedEvent>,
    store: Vec<u8>,
    warnings: Vec<String>,
}

impl Recording {
    /// Разбирает файл целиком (см. правила в документации модуля).
    pub fn parse(bytes: &[u8]) -> Result<Self, RecordingError> {
        let (manifest_bytes, store) = split_file(bytes)?;
        let manifest_json: Value =
            serde_json::from_slice(manifest_bytes).map_err(RecordingError::ManifestJson)?;
        let manifest_object = manifest_json
            .as_object()
            .ok_or(RecordingError::ManifestField {
                field: "(root)",
                problem: "not an object",
            })?;
        let mut warnings = Vec::new();
        for key in manifest_object.keys() {
            if !KNOWN_MANIFEST_FIELDS.contains(&key.as_str()) {
                warnings.push(format!("manifest: unknown field '{key}' ignored"));
            }
        }
        check_format_and_version(manifest_object)?;
        let manifest = read_manifest(manifest_object)?;
        let raw_events = manifest_object
            .get("events")
            .and_then(Value::as_array)
            .ok_or(RecordingError::ManifestField {
                field: "events",
                problem: "missing or not an array",
            })?;
        let mut events = Vec::with_capacity(raw_events.len());
        for (index, raw) in raw_events.iter().enumerate() {
            if let Some(event) = parse_event(index, raw, store, &mut warnings)? {
                events.push(event);
            }
        }
        warn_if_unordered(&events, &mut warnings);
        Ok(Self {
            manifest,
            events,
            store: store.to_vec(),
            warnings,
        })
    }

    /// События в порядке записи (только известные операции).
    pub fn events(&self) -> &[RecordedEvent] {
        &self.events
    }

    /// Предупреждения разбора: пропущенные операции и поля, нарушение порядка времени.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Байты нагрузки события `append` (пусто для остальных операций).
    pub fn payload_of(&self, event: &RecordedEvent) -> &[u8] {
        event
            .payload
            .clone()
            .map_or(&[][..], |range| &self.store[range])
    }

    /// Размер склада байтов.
    pub fn store_length(&self) -> usize {
        self.store.len()
    }

    /// Момент первого `handoff_began`, мс от старта записи.
    pub fn handoff_began_at_ms(&self) -> Option<f64> {
        self.events
            .iter()
            .find(|event| event.op == "handoff_began")
            .map(|event| event.t_ms)
    }

    /// Момент последнего события записи, мс (0 для пустой записи).
    pub fn duration_ms(&self) -> f64 {
        self.events
            .iter()
            .map(|event| event.t_ms)
            .fold(0.0, f64::max)
    }

    /// Есть ли события не из главного потока (воркеры: в Firefox повтор невозможен).
    pub fn uses_worker_context(&self) -> bool {
        self.events.iter().any(|event| event.ctx != MAIN_CONTEXT)
    }
}

/// Делит файл на манифест и склад, проверяя магию и длину.
fn split_file(bytes: &[u8]) -> Result<(&[u8], &[u8]), RecordingError> {
    if bytes.len() < HEADER_LENGTH {
        return Err(RecordingError::TooShort {
            length: bytes.len(),
            needed: HEADER_LENGTH,
        });
    }
    let (magic, rest) = bytes.split_at(MAGIC.len());
    if magic != MAGIC {
        let mut found = [0_u8; 8];
        found.copy_from_slice(magic);
        return Err(RecordingError::BadMagic { found });
    }
    let (length_bytes, after_header) = rest.split_at(4);
    let declared = u32::from_le_bytes([
        length_bytes[0],
        length_bytes[1],
        length_bytes[2],
        length_bytes[3],
    ]) as usize;
    if declared > after_header.len() {
        return Err(RecordingError::ManifestOutOfFile {
            declared,
            available: after_header.len(),
        });
    }
    Ok(after_header.split_at(declared))
}

fn check_format_and_version(manifest: &Map<String, Value>) -> Result<(), RecordingError> {
    let format =
        manifest
            .get("format")
            .and_then(Value::as_str)
            .ok_or(RecordingError::ManifestField {
                field: "format",
                problem: "missing or not a string",
            })?;
    if format != FORMAT_NAME {
        return Err(RecordingError::UnsupportedFormat {
            found: format.to_owned(),
            expected: FORMAT_NAME,
        });
    }
    let version =
        manifest
            .get("version")
            .and_then(Value::as_u64)
            .ok_or(RecordingError::ManifestField {
                field: "version",
                problem: "missing or not an integer",
            })?;
    if version != FORMAT_VERSION {
        return Err(RecordingError::UnsupportedVersion {
            found: version,
            expected: FORMAT_VERSION,
        });
    }
    Ok(())
}

fn read_manifest(manifest: &Map<String, Value>) -> Result<RecordingManifest, RecordingError> {
    let text = |field: &'static str| {
        manifest
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(RecordingError::ManifestField {
                field,
                problem: "missing or not a string",
            })
    };
    Ok(RecordingManifest {
        browser: text("browser")?,
        origin: text("origin")?,
        started_at_unix_ms: manifest
            .get("started_at_unix_ms")
            .and_then(Value::as_u64)
            .ok_or(RecordingError::ManifestField {
                field: "started_at_unix_ms",
                problem: "missing or not an unsigned integer",
            })?,
        truncated: manifest.get("truncated").and_then(Value::as_bool).ok_or(
            RecordingError::ManifestField {
                field: "truncated",
                problem: "missing or not a boolean",
            },
        )?,
    })
}

/// Разбирает одно событие. `Ok(None)` — операция из будущей версии, пропущена с предупреждением.
fn parse_event(
    index: usize,
    raw: &Value,
    store: &[u8],
    warnings: &mut Vec<String>,
) -> Result<Option<RecordedEvent>, RecordingError> {
    let fields = raw
        .as_object()
        .ok_or(RecordingError::EventNotObject { index })?;
    let common_error = |field, problem| RecordingError::EventCommonField {
        index,
        field,
        problem,
    };
    let t_ms = fields
        .get("t_ms")
        .and_then(Value::as_f64)
        .ok_or_else(|| common_error("t_ms", "missing or not a number"))?;
    let ctx = fields
        .get("ctx")
        .and_then(Value::as_str)
        .ok_or_else(|| common_error("ctx", "missing or not a string"))?;
    let op = fields
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| common_error("op", "missing or not a string"))?;
    let Some(spec) = op_spec(op) else {
        warnings.push(format!("event #{index}: unknown op '{op}' skipped"));
        return Ok(None);
    };
    check_required_fields(index, spec, fields)?;
    warn_unknown_fields(index, spec, fields, warnings);
    let payload = if op == "append" {
        Some(read_append_payload(index, fields, store)?)
    } else {
        None
    };
    Ok(Some(RecordedEvent {
        index,
        t_ms,
        ctx: ctx.to_owned(),
        op: op.to_owned(),
        fields: fields.clone(),
        payload,
    }))
}

fn check_required_fields(
    index: usize,
    spec: &OpSpec,
    fields: &Map<String, Value>,
) -> Result<(), RecordingError> {
    for &field in spec.required {
        if !fields.contains_key(field) {
            return Err(RecordingError::MissingField {
                index,
                op: spec.op.to_owned(),
                field,
            });
        }
    }
    Ok(())
}

fn warn_unknown_fields(
    index: usize,
    spec: &OpSpec,
    fields: &Map<String, Value>,
    warnings: &mut Vec<String>,
) {
    for key in fields.keys() {
        let known = COMMON_EVENT_FIELDS.contains(&key.as_str())
            || spec.required.contains(&key.as_str())
            || spec.optional.contains(&key.as_str());
        if !known {
            warnings.push(format!(
                "event #{index} (op '{}'): unknown field '{key}' ignored",
                spec.op
            ));
        }
    }
}

/// Проверяет, что нагрузка `append` внутри склада и её хеш совпал с записанным.
fn read_append_payload(
    index: usize,
    fields: &Map<String, Value>,
    store: &[u8],
) -> Result<Range<usize>, RecordingError> {
    let integer = |field: &'static str| {
        fields
            .get(field)
            .and_then(Value::as_u64)
            .ok_or(RecordingError::WrongFieldType {
                index,
                op: "append".to_owned(),
                field,
                expected: "an unsigned integer",
            })
    };
    let offset = integer("offset")?;
    let length = integer("length")?;
    let recorded_hash =
        fields
            .get("hash")
            .and_then(Value::as_str)
            .ok_or(RecordingError::WrongFieldType {
                index,
                op: "append".to_owned(),
                field: "hash",
                expected: "a string",
            })?;
    let out_of_store = || RecordingError::PayloadOutOfStore {
        index,
        offset,
        length,
        store_length: store.len(),
    };
    let start = usize::try_from(offset).map_err(|_| out_of_store())?;
    let end = start
        .checked_add(usize::try_from(length).map_err(|_| out_of_store())?)
        .filter(|&end| end <= store.len())
        .ok_or_else(out_of_store)?;
    let computed = fnv1a_32_hex(&store[start..end]);
    if computed != recorded_hash {
        return Err(RecordingError::PayloadHashMismatch {
            index,
            recorded: recorded_hash.to_owned(),
            computed,
        });
    }
    Ok(start..end)
}

/// События обязаны идти по возрастанию `t_ms`; нарушение — предупреждение (повтор сортирует сам).
fn warn_if_unordered(events: &[RecordedEvent], warnings: &mut Vec<String>) {
    if let Some(pair) = events.windows(2).find(|pair| pair[1].t_ms < pair[0].t_ms) {
        warnings.push(format!(
            "event #{} goes back in time ({} ms after {} ms): the replay sorts events by t_ms",
            pair[1].index, pair[1].t_ms, pair[0].t_ms
        ));
    }
}
