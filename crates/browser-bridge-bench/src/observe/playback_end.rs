//! Строка журнала плеера «видео доиграло до конца»: `Browser playback ended presented_frames=N …`.
//!
//! Это сигнал «дошло до конца», который используют сценарии (`entries`, `episodes`, `live-ends`).
//! Число кадров в строке накопительное за сеанс, как и в сводках воспроизведения.

use super::player_log::number_field;

/// Начало строки «плеер дошёл до конца текущего медиа».
pub const PLAYBACK_ENDED_MARKER: &str = "Browser playback ended";

/// Сколько кадров плеер показал к концу медиа; `None` — строка не «конец» или поля нет.
pub fn parse_playback_ended_frames(line: &str) -> Option<u64> {
    if !line.contains(PLAYBACK_ENDED_MARKER) {
        return None;
    }
    number_field(line, "presented_frames")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frames_of_an_ended_line_are_read_exactly() {
        let line = "2026-11-12T10:00:00Z  INFO fastiplayer: Browser playback ended presented_frames=412 taken_frames=413";
        assert_eq!(parse_playback_ended_frames(line), Some(412));
    }

    #[test]
    fn a_summary_a_foreign_line_and_an_ended_line_without_frames_give_nothing() {
        assert_eq!(
            parse_playback_ended_frames("Browser playback summary kind=final presented_frames=412"),
            None
        );
        assert_eq!(parse_playback_ended_frames("Browser playback ended"), None);
        // Поле не должно находиться внутри другого слова.
        assert_eq!(
            parse_playback_ended_frames("Browser playback ended taken_presented_frames=5"),
            None
        );
    }
}
