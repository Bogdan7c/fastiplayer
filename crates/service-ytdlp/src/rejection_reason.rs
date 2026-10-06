//! Причина отказа `yt-dlp`, распознанная по его строкам `ERROR:` без хранения текста.
//!
//! Решение владельца (UX сессия 08): пользователь должен видеть, *почему* сайт не отдал
//! видео — приватное, нужен вход, регион, удалено, сайт не поддерживается. `yt-dlp`
//! сообщает это только текстом в stderr. Stderr может содержать URL с токенами, поэтому
//! текст нигде не сохраняется и не покидает этот модуль: строки разбираются потоково,
//! наружу выходит только `Copy` enum без payload.
//!
//! Метки сверены с исходниками `yt-dlp` 2026.08.19 (`extractor/common.py`:
//! `raise_login_required`, `raise_geo_restricted`, `_login_hint`; `networking/exceptions.py`:
//! `HTTP Error {status}`; `YoutubeDL.report_error`: префикс `ERROR:`). Если новая версия
//! поменяет формулировку, причина честно станет `Unclassified` — общая фраза в UI.

use std::fmt;

/// Префикс, которым `yt-dlp` помечает фатальные ошибки (`YoutubeDL.report_error`).
const ERROR_LINE_MARKER: &[u8] = b"ERROR:";

/// Сколько первых байт каждой строки достаточно для распознавания причины.
///
/// Метка причины стоит в начале сообщения (`ERROR: [extractor] id: Private video…`);
/// хвост длинной строки (URL, подсказки) для классификации не нужен и в память не берётся.
const MAX_CLASSIFIED_LINE_BYTES: usize = 2048;

/// Причина non-zero выхода `yt-dlp` в терминах пользователя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpRejectionReason {
    /// Видео приватное (`Private video`).
    PrivateMedia,
    /// Нужен вход в аккаунт: возрастное ограничение, проверка «не бот», только для
    /// подписчиков (`Sign in to confirm…`, подсказка `--cookies`).
    LoginRequired,
    /// Видео недоступно в регионе пользователя (`GeoRestrictedError`).
    GeoRestricted,
    /// Видео удалено или не существует (`Video unavailable`, HTTP 404/410).
    MediaUnavailable,
    /// Сайт не поддерживается `yt-dlp` (`Unsupported URL`).
    UnsupportedUrl,
    /// Сервер сайта запретил доступ (HTTP 403).
    AccessDenied,
    /// Сайт ограничил частоту запросов (HTTP 429).
    RateLimited,
    /// `yt-dlp` не смог соединиться с сайтом (DNS, отказ соединения, нет маршрута).
    NetworkUnavailable,
    /// Отказ без распознанной метки (или stderr пуст).
    Unclassified,
}

impl fmt::Display for YtDlpRejectionReason {
    /// Короткий диагностический код для логов (не текст для пользователя).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::PrivateMedia => "private-media",
            Self::LoginRequired => "login-required",
            Self::GeoRestricted => "geo-restricted",
            Self::MediaUnavailable => "media-unavailable",
            Self::UnsupportedUrl => "unsupported-url",
            Self::AccessDenied => "access-denied",
            Self::RateLimited => "rate-limited",
            Self::NetworkUnavailable => "network-unavailable",
            Self::Unclassified => "unclassified",
        };
        formatter.write_str(code)
    }
}

/// Правило «метки в тексте ошибки → причина». Проверяются по порядку: первое совпадение
/// побеждает. Порядок важен: «Private video» содержит подсказку «Sign in…», поэтому
/// приватность проверяется раньше входа; HTTP-статус точнее общих «unavailable».
const CLASSIFICATION_RULES: &[(&[&str], YtDlpRejectionReason)] = &[
    (
        &["private video", "this video is private"],
        YtDlpRejectionReason::PrivateMedia,
    ),
    (
        &[
            "sign in to confirm",
            "only available for registered users",
            "--cookies",
            "login required",
            "members-only",
            "available to this channel's members",
        ],
        YtDlpRejectionReason::LoginRequired,
    ),
    (
        &[
            "geo restriction",
            "geo-restricted",
            "available in your country",
            "not available from your location",
        ],
        YtDlpRejectionReason::GeoRestricted,
    ),
    (
        &["http error 404", "http error 410"],
        YtDlpRejectionReason::MediaUnavailable,
    ),
    (&["http error 403"], YtDlpRejectionReason::AccessDenied),
    (
        &["http error 429", "too many requests"],
        YtDlpRejectionReason::RateLimited,
    ),
    (
        &[
            "video unavailable",
            // Ручная приёмка UX08 (2026-10-06): YouTube отдаёт причину текстом сервера,
            // реальный ответ для несуществующего id — «This video is unavailable».
            "video is unavailable",
            "this video is not available",
            "has been removed",
            "no longer available",
            "does not exist",
        ],
        YtDlpRejectionReason::MediaUnavailable,
    ),
    (&["unsupported url"], YtDlpRejectionReason::UnsupportedUrl),
    (
        &[
            "name resolution",
            "name or service not known",
            "failed to resolve",
            "nodename nor servname",
            "getaddrinfo failed",
            "connection refused",
            "network is unreachable",
            "no route to host",
        ],
        YtDlpRejectionReason::NetworkUnavailable,
    ),
];

/// Классифицирует одну строку stderr; `None` — строка не является распознанной ошибкой.
fn classify_error_line(line: &[u8]) -> Option<YtDlpRejectionReason> {
    let marker_start = line
        .windows(ERROR_LINE_MARKER.len())
        .position(|window| window == ERROR_LINE_MARKER)?;
    let message = &line[marker_start + ERROR_LINE_MARKER.len()..];
    // Lossy: битый UTF-8 не должен ронять классификацию; сравнение — без учёта регистра.
    let lowercase_message = String::from_utf8_lossy(message).to_lowercase();
    CLASSIFICATION_RULES
        .iter()
        .find(|(markers, _)| {
            markers
                .iter()
                .any(|marker| lowercase_message.contains(marker))
        })
        .map(|(_, reason)| *reason)
}

/// Потоковый разборщик stderr: держит в памяти не больше одной обрезанной строки.
///
/// Побеждает первая распознанная строка `ERROR:` — `yt-dlp` печатает итоговую ошибку
/// одной строкой, а предупреждения (`WARNING:`) не классифицируются вовсе.
#[derive(Debug, Default)]
pub(crate) struct StderrRejectionClassifier {
    /// Начало текущей строки (не больше `MAX_CLASSIFIED_LINE_BYTES`).
    current_line: Vec<u8>,
    /// Первая распознанная причина.
    recognized_reason: Option<YtDlpRejectionReason>,
}

impl StderrRejectionClassifier {
    /// Принимает очередной кусок stderr произвольной длины.
    pub(crate) fn observe(&mut self, chunk: &[u8]) {
        for line_part in chunk.split_inclusive(|byte| *byte == b'\n') {
            let (content, line_finished) = match line_part.strip_suffix(b"\n") {
                Some(content) => (content, true),
                None => (line_part, false),
            };
            self.append_to_current_line(content);
            if line_finished {
                self.finish_current_line();
            }
        }
    }

    /// Завершает разбор (последняя строка может быть без `\n`) и отдаёт причину.
    pub(crate) fn finish(mut self) -> YtDlpRejectionReason {
        self.finish_current_line();
        self.recognized_reason
            .unwrap_or(YtDlpRejectionReason::Unclassified)
    }

    /// Дописывает байты строки, отбрасывая всё сверх лимита строки.
    fn append_to_current_line(&mut self, content: &[u8]) {
        let free_bytes = MAX_CLASSIFIED_LINE_BYTES.saturating_sub(self.current_line.len());
        let kept_bytes = content.len().min(free_bytes);
        self.current_line.extend_from_slice(&content[..kept_bytes]);
    }

    /// Классифицирует накопленную строку и сразу стирает её текст.
    fn finish_current_line(&mut self) {
        if self.recognized_reason.is_none() {
            self.recognized_reason = classify_error_line(&self.current_line);
        }
        self.current_line.clear();
    }
}

#[cfg(test)]
#[path = "rejection_reason/tests.rs"]
mod tests;
