//! Разбор строк журнала плеера про субтитры страницы (сессия 13b2).
//!
//! Плеер пишет (tracing: сообщение, потом поля `имя=значение`):
//! - `Page subtitle document received track=1 bytes=7099 format_hint=… page_display=… replaced=false`
//!   — склад сеанса принял документ (`replaced=true` — тот же номер дорожки заменён новым);
//! - `Page subtitle track ready track=1 language=en format=web_vtt cues=150 selected=true`
//!   — дорожка разобрана и готова к показу;
//! - `Browser playback summary … subtitle_overlay_frames=N` — на скольких кадрах был слой субтитров.
//!
//! Всё здесь — чистые функции над строками: проверяются тестами на настоящей форме строки.

use super::player_log::{number_field, word_field};

/// Метка строки «дорожка страницы готова».
pub const PAGE_SUBTITLE_TRACK_READY_MARKER: &str = "Page subtitle track ready";
/// Метка строки «склад принял документ».
pub const PAGE_SUBTITLE_DOCUMENT_MARKER: &str = "Page subtitle document received";
/// Метка сводки воспроизведения (там живёт `subtitle_overlay_frames`).
const SUMMARY_MARKER: &str = "Browser playback summary";

/// Строка «дорожка страницы готова».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSubtitleTrackLine {
    /// Номер дорожки на сеанс (его выдаёт экстеншен; тот же номер = замена документа).
    pub track: u64,
    /// Тег языка (`en`) или `unknown`.
    pub language: String,
    /// Формат документа в метке журнала (`web_vtt`, `srt`, `json3`…).
    pub format: String,
    /// Сколько реплик разобрано.
    pub cues: u64,
    /// Включена ли дорожка (плеер показывает именно её).
    pub selected: bool,
}

/// Строка «склад принял документ».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSubtitleDocumentLine {
    /// Номер дорожки.
    pub track: u64,
    /// Подсказка формата от страницы/экстеншена (`web_vtt`, `srt`, `json3`…).
    pub format_hint: String,
    /// Показывала ли дорожку сама страница (`page_display="showing"`).
    pub page_showing: bool,
    /// Заменил ли документ прежний той же дорожки.
    pub replaced: bool,
}

/// Булево поле `name=true|false`; `None`, если поля нет или значение другое.
fn bool_field(line: &str, name: &str) -> Option<bool> {
    match word_field(line, name)?.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Строковое поле `name="значение"` (tracing берёт строки в кавычки) или `name=значение` без них.
fn text_field(line: &str, name: &str) -> Option<String> {
    let raw = word_field(line, name)?;
    let unquoted = raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'));
    Some(unquoted.unwrap_or(&raw).to_owned())
}

/// Разбор строки `Page subtitle track ready …`; `None`, если строка не такая или поле потеряно.
pub fn parse_page_subtitle_track_ready(line: &str) -> Option<PageSubtitleTrackLine> {
    if !line.contains(PAGE_SUBTITLE_TRACK_READY_MARKER) {
        return None;
    }
    Some(PageSubtitleTrackLine {
        track: number_field(line, "track")?,
        language: text_field(line, "language")?,
        format: text_field(line, "format")?,
        cues: number_field(line, "cues")?,
        selected: bool_field(line, "selected")?,
    })
}

/// Разбор строки `Page subtitle document received …`; `None`, если строка не такая.
pub fn parse_page_subtitle_document(line: &str) -> Option<PageSubtitleDocumentLine> {
    if !line.contains(PAGE_SUBTITLE_DOCUMENT_MARKER) {
        return None;
    }
    Some(PageSubtitleDocumentLine {
        track: number_field(line, "track")?,
        format_hint: text_field(line, "format_hint")?,
        page_showing: match text_field(line, "page_display")?.as_str() {
            "showing" => true,
            "not_showing" => false,
            _ => return None,
        },
        replaced: bool_field(line, "replaced")?,
    })
}

/// Наибольшее `subtitle_overlay_frames` среди сводок (счётчик накопительный). `None` — ни одной
/// сводки с этим полем: это отличается от «слой был на 0 кадрах».
pub fn max_subtitle_overlay_frames(summary_lines: &[String]) -> Option<u64> {
    summary_lines
        .iter()
        .filter(|line| line.contains(SUMMARY_MARKER))
        .filter_map(|line| number_field(line, "subtitle_overlay_frames"))
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    const READY: &str = "2026-11-09T10:00:00Z  INFO fastiplayer: Page subtitle track ready track=2 language=\"uk\" format=\"web_vtt\" cues=150 selected=false";
    const DOCUMENT: &str = "2026-11-09T10:00:00Z  INFO fastiplayer: Page subtitle document received track=1 bytes=7099 format_hint=\"web_vtt\" page_display=\"not_showing\" replaced=true";

    #[test]
    fn a_ready_line_gives_track_language_cues_and_selection() {
        assert_eq!(
            parse_page_subtitle_track_ready(READY),
            Some(PageSubtitleTrackLine {
                track: 2,
                language: "uk".to_owned(),
                format: "web_vtt".to_owned(),
                cues: 150,
                selected: false,
            })
        );
    }

    /// Настоящие строки из `player.log` прогона (со временем, INFO и кавычками tracing).
    #[test]
    fn real_player_log_lines_are_parsed() {
        let ready = "2026-11-09T08:50:48.702101Z  INFO Page subtitle track ready track=1 language=\"en\" format=\"web_vtt\" cues=20 selected=true";
        let track = parse_page_subtitle_track_ready(ready).expect("ready line");
        assert_eq!((track.track, track.language.as_str()), (1, "en"));
        assert_eq!(
            (track.format.as_str(), track.cues, track.selected),
            ("web_vtt", 20, true)
        );
        let document = "2026-11-09T08:49:15.236701Z  INFO Page subtitle document received track=1 bytes=7399 format_hint=\"srt\" page_display=\"not_showing\" replaced=false";
        let parsed = parse_page_subtitle_document(document).expect("document line");
        assert_eq!(parsed.format_hint, "srt");
        assert!(!parsed.page_showing && !parsed.replaced);
        let showing = document.replace("not_showing", "showing");
        assert!(
            parse_page_subtitle_document(&showing)
                .expect("showing")
                .page_showing
        );
    }

    #[test]
    fn a_selected_ready_line_is_selected() {
        let line = READY.replace("selected=false", "selected=true");
        assert!(
            parse_page_subtitle_track_ready(&line)
                .expect("parsed")
                .selected
        );
    }

    #[test]
    fn ready_lines_with_a_lost_field_or_another_text_are_not_parsed() {
        assert_eq!(
            parse_page_subtitle_track_ready(&READY.replace(" cues=150", "")),
            None
        );
        assert_eq!(
            parse_page_subtitle_track_ready(&READY.replace("selected=false", "selected=maybe")),
            None
        );
        assert_eq!(parse_page_subtitle_track_ready(DOCUMENT), None);
    }

    #[test]
    fn a_document_line_tells_whether_it_replaced_the_track() {
        assert_eq!(
            parse_page_subtitle_document(DOCUMENT),
            Some(PageSubtitleDocumentLine {
                track: 1,
                format_hint: "web_vtt".to_owned(),
                page_showing: false,
                replaced: true
            })
        );
        let fresh = DOCUMENT.replace("replaced=true", "replaced=false");
        assert!(
            !parse_page_subtitle_document(&fresh)
                .expect("parsed")
                .replaced
        );
        assert_eq!(parse_page_subtitle_document(READY), None);
    }

    #[test]
    fn the_overlay_counter_is_the_maximum_over_summaries() {
        let summaries = vec![
            "INFO x: Browser playback summary kind=periodic presented_frames=250 subtitle_overlay_frames=0".to_owned(),
            "INFO x: Browser playback summary kind=periodic presented_frames=500 subtitle_overlay_frames=240".to_owned(),
            "INFO x: Browser playback summary kind=final presented_frames=700 subtitle_overlay_frames=130".to_owned(),
        ];
        assert_eq!(max_subtitle_overlay_frames(&summaries), Some(240));
    }

    #[test]
    fn no_summary_with_the_counter_is_not_zero_frames() {
        assert_eq!(max_subtitle_overlay_frames(&[]), None);
        let old =
            vec!["INFO x: Browser playback summary kind=periodic presented_frames=1".to_owned()];
        assert_eq!(max_subtitle_overlay_frames(&old), None);
    }
}
