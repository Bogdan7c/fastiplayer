//! Отдача файлов с диска: Content-Type, `Range`, 404/416. Знает про файлы, но не про маршруты.

use std::fs::File;
use std::path::Path;

use super::http::{Body, Response};

/// Что просит клиент заголовком `Range`.
#[derive(Debug, PartialEq, Eq)]
enum RangeRequest {
    /// Заголовка нет или он не про байты / несколько диапазонов: отдаём файл целиком (RFC 9110 это разрешает).
    Whole,
    /// Один корректный диапазон: включительно `[first, last]`.
    Slice { first: u64, last: u64 },
    /// Диапазон за пределами файла или синтаксически битый → 416.
    Unsatisfiable,
}

/// Тип содержимого по расширению. Для текстовых типов добавляется charset.
pub(super) fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "vtt" => "text/vtt; charset=utf-8",
        "m3u8" => "application/vnd.apple.mpegurl",
        "mp4" | "m4s" | "m4v" => "video/mp4",
        // Сегменты WebM стенд называет .chk (так их называет ffmpeg dash).
        "webm" | "chk" => "video/webm",
        "ts" => "video/mp2t",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Отдаёт файл по пути (уже проверенному на безопасность) с учётом заголовка `Range`.
pub(super) fn serve_file(path: &Path, range_header: Option<&str>) -> Response {
    let (file, size) = match open_with_size(path) {
        Ok(opened) => opened,
        Err(_) => return Response::text(404, "not found"),
    };
    let base = |status: u16| {
        Response::new(status)
            .with_header("Content-Type", content_type_for(path))
            .with_header("Accept-Ranges", "bytes")
            // Страницы стенда правятся на ходу — браузеру нечего кэшировать.
            .with_header("Cache-Control", "no-store")
    };
    match parse_range(range_header, size) {
        RangeRequest::Whole => base(200).with_body(Body::FileRange {
            file,
            offset: 0,
            length: size,
        }),
        RangeRequest::Slice { first, last } => base(206)
            .with_header("Content-Range", format!("bytes {first}-{last}/{size}"))
            .with_body(Body::FileRange {
                file,
                offset: first,
                length: last - first + 1,
            }),
        RangeRequest::Unsatisfiable => {
            base(416).with_header("Content-Range", format!("bytes */{size}"))
        }
    }
}

fn open_with_size(path: &Path) -> std::io::Result<(File, u64)> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    Ok((file, size))
}

fn parse_range(header: Option<&str>, size: u64) -> RangeRequest {
    let Some(value) = header else {
        return RangeRequest::Whole;
    };
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return RangeRequest::Whole;
    };
    if spec.contains(',') {
        // Несколько диапазонов нужны только странным клиентам; полный файл — законный ответ.
        return RangeRequest::Whole;
    }
    let Some((start_text, end_text)) = spec.split_once('-') else {
        return RangeRequest::Unsatisfiable;
    };
    let (start_text, end_text) = (start_text.trim(), end_text.trim());
    if size == 0 {
        return RangeRequest::Unsatisfiable;
    }
    let last_byte = size - 1;
    if start_text.is_empty() {
        // `bytes=-N` — последние N байт.
        return match end_text.parse::<u64>() {
            Ok(suffix) if suffix > 0 => RangeRequest::Slice {
                first: size.saturating_sub(suffix),
                last: last_byte,
            },
            _ => RangeRequest::Unsatisfiable,
        };
    }
    let Ok(first) = start_text.parse::<u64>() else {
        return RangeRequest::Unsatisfiable;
    };
    let last = if end_text.is_empty() {
        last_byte
    } else {
        match end_text.parse::<u64>() {
            Ok(end) => end.min(last_byte),
            Err(_) => return RangeRequest::Unsatisfiable,
        }
    };
    if first > last_byte || first > last {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Slice { first, last }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_forms_parse_to_expected_slices() {
        assert_eq!(
            parse_range(Some("bytes=0-9"), 100),
            RangeRequest::Slice { first: 0, last: 9 }
        );
        assert_eq!(
            parse_range(Some("bytes=90-"), 100),
            RangeRequest::Slice {
                first: 90,
                last: 99
            }
        );
        assert_eq!(
            parse_range(Some("bytes=-10"), 100),
            RangeRequest::Slice {
                first: 90,
                last: 99
            }
        );
        // Конец за границей файла обрезается, а не считается ошибкой (RFC 9110).
        assert_eq!(
            parse_range(Some("bytes=95-500"), 100),
            RangeRequest::Slice {
                first: 95,
                last: 99
            }
        );
        assert_eq!(parse_range(None, 100), RangeRequest::Whole);
        assert_eq!(parse_range(Some("items=1-2"), 100), RangeRequest::Whole);
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), RangeRequest::Whole);
    }

    #[test]
    fn unsatisfiable_ranges_are_detected() {
        assert_eq!(
            parse_range(Some("bytes=100-"), 100),
            RangeRequest::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=9-3"), 100),
            RangeRequest::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=abc-"), 100),
            RangeRequest::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=-0"), 100),
            RangeRequest::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=0-1"), 0),
            RangeRequest::Unsatisfiable
        );
    }

    #[test]
    fn content_types_match_the_contract() {
        assert_eq!(
            content_type_for(Path::new("a.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type_for(Path::new("a.m3u8")),
            "application/vnd.apple.mpegurl"
        );
        assert_eq!(content_type_for(Path::new("a.m4s")), "video/mp4");
        assert_eq!(
            content_type_for(Path::new("a.unknown")),
            "application/octet-stream"
        );
    }
}
