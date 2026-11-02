//! Ошибки подготовки медиа. Тексты на английском: они уходят в вывод стенда.

use std::path::PathBuf;

/// Всё, что может пойти не так при генерации, чтении и проверке каталога.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// Операция с файловой системой не удалась.
    #[error("cannot {action} ({path}): {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Скрипт генерации не удалось даже запустить (нет bash и т. п.).
    #[error("cannot start the media generator script {script}: {source}")]
    GeneratorSpawn {
        script: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Скрипт отработал с ошибкой (нет кодера ffmpeg, упал ffmpeg, проверка сегментов).
    #[error("media generation failed (exit status {status}):\n{stderr_tail}")]
    GeneratorFailed { status: String, stderr_tail: String },
    /// `catalog.json` не разобрался в типизированную структуру.
    #[error("catalog {path} is not valid: {source}")]
    CatalogParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// Каталог новее или старше, чем понимает этот код.
    #[error("unsupported catalog version {found}, expected {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },
    /// Каталог ссылается на файл, которого нет (или он пустой).
    #[error("catalog asset '{asset}' references a missing or empty file: {url}")]
    MissingFile { asset: String, url: String },
    /// `url` в каталоге выходит за корень или написан небезопасно.
    #[error("catalog asset '{asset}' has an unsafe url: {url}")]
    UnsafeUrl { asset: String, url: String },
}
