//! Константы формата `*.fprec` и таблица известных операций.
//!
//! Таблица — единственное место, где перечислены `op` и их поля. Разбор по ней отличает
//! «обязательное поле потеряно» (ошибка) от «поле/операция из будущей версии» (предупреждение).

/// Магия в начале файла.
pub const MAGIC: &[u8; 8] = b"FPREC001";
/// Размер заголовка: магия и u32 длины манифеста.
pub const HEADER_LENGTH: usize = 12;
/// Значение `format` в манифесте.
pub const FORMAT_NAME: &str = "fastiplayer-page-stream-recording";
/// Версия формата, которую знает читатель.
pub const FORMAT_VERSION: u64 = 1;
/// Контекст главного потока страницы.
pub const MAIN_CONTEXT: &str = "main";

/// Поля манифеста, которые читатель знает (остальные — предупреждение).
pub const KNOWN_MANIFEST_FIELDS: &[&str] = &[
    "format",
    "version",
    "browser",
    "origin",
    "started_at_unix_ms",
    "truncated",
    "events",
];
/// Общие поля каждого события.
pub const COMMON_EVENT_FIELDS: &[&str] = &["t_ms", "ctx", "op"];

/// Описание одной операции: её имя, обязательные и необязательные поля.
#[derive(Debug, Clone, Copy)]
pub struct OpSpec {
    pub op: &'static str,
    pub required: &'static [&'static str],
    pub optional: &'static [&'static str],
}

const fn spec(op: &'static str, required: &'static [&'static str]) -> OpSpec {
    OpSpec {
        op,
        required,
        optional: &[],
    }
}

/// Все операции формата версии 1 (см. бриф сессии 09b2).
pub const OPS: &[OpSpec] = &[
    spec("media_source_created", &["ms"]),
    spec("media_source_object_url", &["ms"]),
    spec("media_source_handle", &["ms"]),
    spec("source_open", &["ms"]),
    spec("source_ended", &["ms"]),
    spec("source_close", &["ms"]),
    OpSpec {
        op: "element_source",
        required: &["el", "kind"],
        optional: &["ms"],
    },
    spec("add_source_buffer", &["ms", "sb", "mime"]),
    spec("remove_source_buffer", &["ms", "sb"]),
    spec("append", &["ms", "sb", "offset", "length", "hash"]),
    spec("append_error", &["ms", "sb", "error"]),
    spec("update_end", &["ms", "sb"]),
    spec("remove", &["ms", "sb", "start", "end"]),
    spec("abort", &["ms", "sb"]),
    spec("change_type", &["ms", "sb", "mime"]),
    spec("timestamp_offset", &["ms", "sb", "value"]),
    spec("append_window_start", &["ms", "sb", "value"]),
    spec("append_window_end", &["ms", "sb", "value"]),
    spec("mode", &["ms", "sb", "value"]),
    spec("duration", &["ms", "value"]),
    OpSpec {
        op: "end_of_stream",
        required: &["ms"],
        optional: &["error"],
    },
    spec("live_seekable_range", &["ms", "start", "end"]),
    spec("live_seekable_range_cleared", &["ms"]),
    spec("buffered", &["ms", "sb", "ranges"]),
    spec("element_play", &["el"]),
    spec("element_pause", &["el"]),
    spec("element_seek", &["el", "value"]),
    spec("element_rate", &["el", "value"]),
    spec("element_load", &["el"]),
    spec("handoff_began", &["el"]),
    spec("handoff_replayed", &["items", "bytes"]),
    spec("handoff_ended", &["cause"]),
    spec("page_command", &["command"]),
    spec("sent", &["ms", "sb", "length", "hash", "replay"]),
];

/// Описание операции по имени; `None` — операция из будущей версии.
pub fn op_spec(op: &str) -> Option<&'static OpSpec> {
    OPS.iter().find(|spec| spec.op == op)
}

/// FNV-1a 32 бит над байтами — тот же хеш, что считает рекордер экстеншена.
pub fn fnv1a_32(bytes: &[u8]) -> u32 {
    const OFFSET_BASIS: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;
    bytes.iter().fold(OFFSET_BASIS, |hash, &byte| {
        (hash ^ u32::from(byte)).wrapping_mul(PRIME)
    })
}

/// Хеш в виде 8 hex-цифр строчными (так он записан в `append.hash`).
pub fn fnv1a_32_hex(bytes: &[u8]) -> String {
    format!("{:08x}", fnv1a_32(bytes))
}
