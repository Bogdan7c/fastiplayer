//! Разбор строки журнала плеера «качество, которое показывает окно» (сессия 13c).
//!
//! Плеер пишет при каждой смене подписи (формат фиксирован, его читает стенд):
//! `Browser quality shown label=360p width=640 height=360 bitrate_bps=500000`
//! или `bitrate_bps=unknown`, когда «вес потока» неизвестен (HLS, файл `<video src>`).
//!
//! Всё здесь — чистые функции над строками.

use super::player_log::{number_field, word_field};

/// Метка строки «окно показало качество».
pub const BROWSER_QUALITY_SHOWN_MARKER: &str = "Browser quality shown";

/// Битрейт («вес потока») в строке: число или честное «неизвестно».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityBitrate {
    /// Известен, бит/с.
    Known(u64),
    /// Плеер написал `unknown`.
    Unknown,
}

/// Строка «окно показало качество».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserQualityLine {
    /// Подпись ступени (`360p`, `720p`…).
    pub label: String,
    /// Ширина кадра.
    pub width: u64,
    /// Высота кадра.
    pub height: u64,
    /// Вес потока.
    pub bitrate: QualityBitrate,
}

/// Разбор строки `Browser quality shown …`; `None`, если строка не такая, поле потеряно или
/// битрейт — ни число, ни `unknown`.
pub fn parse_browser_quality_shown(line: &str) -> Option<BrowserQualityLine> {
    if !line.contains(BROWSER_QUALITY_SHOWN_MARKER) {
        return None;
    }
    let bitrate = match word_field(line, "bitrate_bps")?.as_str() {
        "unknown" => QualityBitrate::Unknown,
        _ => QualityBitrate::Known(number_field(line, "bitrate_bps")?),
    };
    Some(BrowserQualityLine {
        label: word_field(line, "label")?,
        width: number_field(line, "width")?,
        height: number_field(line, "height")?,
        bitrate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_line_with_a_known_bitrate() {
        let line = "2026-11-09T10:00:00Z  INFO Browser quality shown label=720p width=1280 height=720 bitrate_bps=1500000";
        assert_eq!(
            parse_browser_quality_shown(line),
            Some(BrowserQualityLine {
                label: "720p".to_owned(),
                width: 1280,
                height: 720,
                bitrate: QualityBitrate::Known(1_500_000),
            })
        );
    }

    #[test]
    fn parses_an_unknown_bitrate() {
        let line = "Browser quality shown label=360p width=640 height=360 bitrate_bps=unknown";
        let parsed = parse_browser_quality_shown(line).expect("line parses");
        assert_eq!(parsed.bitrate, QualityBitrate::Unknown);
        assert_eq!(parsed.label, "360p");
    }

    #[test]
    fn garbage_and_foreign_lines_are_ignored() {
        for line in [
            "",
            "Browser playback summary presented_frames=10",
            // Метка есть, но поле потеряно.
            "Browser quality shown label=360p width=640 bitrate_bps=1",
            // Битрейт — не число и не `unknown`.
            "Browser quality shown label=360p width=640 height=360 bitrate_bps=fast",
            "Browser quality shown label=360p width=640 height=360",
        ] {
            assert_eq!(parse_browser_quality_shown(line), None, "{line:?}");
        }
    }
}
