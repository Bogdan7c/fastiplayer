//! Разбор журнала плеера для `live-ends`: что случилось ДО перехода «эфир -> запись», а что ПОСЛЕ.
//!
//! Журнал приходит уже отфильтрованным и в порядке записи (см. [`JOURNAL_NEEDLES`]). Здесь чистые
//! функции: ни файлов, ни браузера, поэтому всё проверяется тестами на синтетических журналах.
//!
//! Кадры считает накопительный счётчик сводок (`presented_frames`), поэтому «кадры выросли после
//! перехода» = наибольшее значение счётчика ПОСЛЕ строки перехода минус последнее значение ДО неё.

use crate::observe::{PLAYBACK_ENDED_MARKER, parse_playback_ended_frames, parse_summary};

/// Строка плеера: шкала эфира стала записью (появилась длина).
pub(super) const RECORDING_MARKER: &str = "Live timeline became a recording";
/// Начало строки сводки воспроизведения (по ней собирается журнал; разбирает `parse_summary`).
const SUMMARY_MARKER: &str = "Browser playback summary";
/// Строки плеера, означающие, что поток отвергнут или воспроизведение упало (`generation_rejection.rs`).
pub(super) const REJECTION_MARKERS: &[&str] = &[
    "Stream generation rejected",
    "Browser stream playback failed",
    "protocol_violation",
];

/// Подстроки, по которым сценарий отбирает строки журнала плеера (порядок строк сохраняется).
pub(super) fn journal_needles() -> Vec<&'static str> {
    let mut needles = vec![SUMMARY_MARKER, PLAYBACK_ENDED_MARKER, RECORDING_MARKER];
    needles.extend_from_slice(REJECTION_MARKERS);
    needles
}

/// Факты журнала о переходе «эфир -> запись».
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TransitionFacts {
    /// Сколько кадров было показано к последней сводке ДО строки перехода (0 — сводок не было).
    pub frames_before: u64,
    /// Наибольший счётчик кадров ПОСЛЕ строки перехода (`None` — после неё нет ни сводки, ни конца).
    pub frames_after: Option<u64>,
    /// Плеер дошёл до конца записи (`Browser playback ended` ПОСЛЕ строки перехода).
    pub reached_the_end: bool,
}

impl TransitionFacts {
    /// На сколько выросло число кадров после перехода (0, если после него кадров не видно).
    pub(super) fn frames_gained(&self) -> u64 {
        self.frames_after
            .map_or(0, |after| after.saturating_sub(self.frames_before))
    }
}

/// Что журнал сказал о переходе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Transition {
    /// Строки перехода нет: шкала так и осталась эфиром.
    Missing,
    /// Строка перехода есть.
    Seen(TransitionFacts),
}

/// Читает журнал: находит ПЕРВУЮ строку перехода и считает кадры вокруг неё.
pub(super) fn read_transition(journal: &[String]) -> Transition {
    let Some(index) = journal
        .iter()
        .position(|line| line.contains(RECORDING_MARKER))
    else {
        return Transition::Missing;
    };
    let frames_before = journal[..index]
        .iter()
        .filter_map(|line| parse_summary(line))
        .map(|summary| summary.presented_frames)
        .next_back()
        .unwrap_or(0);
    let after = &journal[index + 1..];
    let frames_after = after
        .iter()
        .filter_map(|line| {
            parse_summary(line)
                .map(|summary| summary.presented_frames)
                .or_else(|| parse_playback_ended_frames(line))
        })
        .max();
    let reached_the_end = after
        .iter()
        .any(|line| parse_playback_ended_frames(line).is_some());
    Transition::Seen(TransitionFacts {
        frames_before,
        frames_after,
        reached_the_end,
    })
}

/// Строки журнала об отказе потока (пусто — отказов нет).
pub(super) fn rejection_lines(journal: &[String]) -> Vec<&String> {
    journal
        .iter()
        .filter(|line| REJECTION_MARKERS.iter().any(|marker| line.contains(marker)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(frames: u64) -> String {
        format!(
            "INFO fastiplayer: Browser playback summary kind=periodic presented_frames={frames} position_regressions=0"
        )
    }

    fn ended(frames: u64) -> String {
        format!("INFO fastiplayer: Browser playback ended presented_frames={frames}")
    }

    fn recording() -> String {
        "INFO fastiplayer: Live timeline became a recording duration_ms=19000 available_from_ms=0"
            .to_owned()
    }

    #[test]
    fn frames_before_are_the_last_summary_and_frames_after_the_highest_counter() {
        let journal = vec![
            summary(100),
            summary(300),
            recording(),
            summary(450),
            ended(520),
        ];
        let Transition::Seen(facts) = read_transition(&journal) else {
            panic!("the transition is in the journal");
        };
        assert_eq!(facts.frames_before, 300);
        assert_eq!(facts.frames_after, Some(520));
        assert_eq!(facts.frames_gained(), 220);
        assert!(facts.reached_the_end);
    }

    #[test]
    fn a_journal_without_the_recording_line_is_missing_the_transition() {
        let journal = vec![summary(100), summary(300), ended(400)];
        assert_eq!(read_transition(&journal), Transition::Missing);
    }

    #[test]
    fn an_end_line_before_the_transition_does_not_count_as_playing_to_the_end() {
        // Конец ДО перехода - это конец другого медиа, а не записи.
        let journal = vec![summary(100), ended(150), recording(), summary(160)];
        let Transition::Seen(facts) = read_transition(&journal) else {
            panic!("the transition is in the journal");
        };
        assert!(!facts.reached_the_end);
        assert_eq!(facts.frames_before, 100);
        assert_eq!(facts.frames_after, Some(160));
    }

    #[test]
    fn nothing_after_the_transition_means_no_frames_and_no_end() {
        let journal = vec![summary(100), recording()];
        let Transition::Seen(facts) = read_transition(&journal) else {
            panic!("the transition is in the journal");
        };
        assert_eq!(facts.frames_after, None);
        assert_eq!(facts.frames_gained(), 0);
        assert!(!facts.reached_the_end);
    }

    #[test]
    fn rejection_lines_are_found_among_the_others() {
        let journal = vec![
            summary(100),
            "WARN fastiplayer: Stream generation rejected reason=unsupported_codec".to_owned(),
            ended(400),
        ];
        let found = rejection_lines(&journal);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("Stream generation rejected"));
        assert!(rejection_lines(&[summary(1), recording(), ended(2)]).is_empty());
    }

    #[test]
    fn the_needles_select_every_line_the_judge_reads() {
        let needles = journal_needles();
        for line in [
            summary(1),
            ended(2),
            recording(),
            "Stream generation rejected".to_owned(),
            "Browser stream playback failed".to_owned(),
        ] {
            assert!(
                needles.iter().any(|needle| line.contains(needle)),
                "no needle for {line}"
            );
        }
    }
}
