//! Разбор журнала плеера для `hls-live-sleep`: дождался ли плеер сеть, склеил ли окно эфира и
//! сколько показал после склейки.
//!
//! Журнал приходит уже отфильтрованным и в порядке записи (см. [`journal_needles`]). Функции чистые:
//! ни файлов, ни браузера — проверяются тестами на синтетических журналах.
//!
//! Кадры считает накопительный счётчик сводок (`presented_frames`), поэтому «кадры после склейки» =
//! наибольший счётчик ПОСЛЕ строки склейки минус последний счётчик ДО неё.

use crate::observe::parse_summary;

/// Строка движка HLS (`web-media-hls`, `live/refresh.rs`): окно эфира без общего сегмента склеено с
/// прочитанным (решение 333).
pub(super) const REJOIN_MARKER: &str =
    "HLS live lost its place in the playlist window and rejoined near the live edge";
/// Строка движка HLS (`live/network_outage.rs`): после серии сетевых сбоев ресурс снова скачался
/// (решение 334).
pub(super) const NETWORK_RECOVERED_MARKER: &str = "HLS live network recovered after";
/// Начало строки сводки воспроизведения (её разбирает `parse_summary`).
const SUMMARY_MARKER: &str = "Browser playback summary";
/// Строки плеера, означающие, что поток отвергнут или воспроизведение упало (те же, что у
/// `live-ends`: `generation_rejection.rs`).
const REJECTION_MARKERS: &[&str] = &[
    "Stream generation rejected",
    "Browser stream playback failed",
    "protocol_violation",
];

/// Подстроки, по которым сценарий отбирает строки журнала плеера (порядок строк сохраняется).
pub(super) fn journal_needles() -> Vec<&'static str> {
    let mut needles = vec![SUMMARY_MARKER, REJOIN_MARKER, NETWORK_RECOVERED_MARKER];
    needles.extend_from_slice(REJECTION_MARKERS);
    needles
}

/// Что журнал плеера сказал про «сон» сервера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SleepJournal {
    /// Строка «сеть вернулась» есть: плеер переждал сбои, а не упал на первом.
    pub network_recovered: bool,
    /// Склейка окна; `None` — строки склейки нет.
    pub rejoin: Option<RejoinFacts>,
    /// Наибольшее число откатов позиции по всем сводкам (склейка не должна двигать часы назад).
    pub max_position_regressions: u64,
    /// Строки отказа потока (поколение отвергнуто, воспроизведение упало).
    pub rejections: Vec<String>,
}

/// Кадры вокруг ПЕРВОЙ строки склейки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RejoinFacts {
    /// Сколько кадров показано к последней сводке ДО склейки (0 — сводок не было).
    pub frames_before: u64,
    /// Наибольший счётчик кадров ПОСЛЕ склейки (`None` — после неё нет ни одной сводки).
    pub frames_after: Option<u64>,
}

impl RejoinFacts {
    /// На сколько выросло число кадров после склейки (0, если после неё сводок нет).
    pub(super) fn frames_gained(self) -> u64 {
        self.frames_after
            .map_or(0, |after| after.saturating_sub(self.frames_before))
    }
}

/// Читает отобранный журнал плеера.
pub(super) fn read_sleep_journal(journal: &[String]) -> SleepJournal {
    let rejoin_index = journal.iter().position(|line| line.contains(REJOIN_MARKER));
    let rejoin = rejoin_index.map(|index| rejoin_facts(journal, index));
    let max_position_regressions = journal
        .iter()
        .filter_map(|line| parse_summary(line))
        .map(|summary| summary.position_regressions)
        .max()
        .unwrap_or(0);
    let rejections = journal
        .iter()
        .filter(|line| REJECTION_MARKERS.iter().any(|marker| line.contains(marker)))
        .cloned()
        .collect();
    SleepJournal {
        network_recovered: journal
            .iter()
            .any(|line| line.contains(NETWORK_RECOVERED_MARKER)),
        rejoin,
        max_position_regressions,
        rejections,
    }
}

/// Кадры до и после строки склейки с номером `rejoin_index`.
fn rejoin_facts(journal: &[String], rejoin_index: usize) -> RejoinFacts {
    let frames_before = journal[..rejoin_index]
        .iter()
        .rev()
        .find_map(|line| parse_summary(line))
        .map_or(0, |summary| summary.presented_frames);
    let frames_after = journal[rejoin_index + 1..]
        .iter()
        .filter_map(|line| parse_summary(line))
        .map(|summary| summary.presented_frames)
        .max();
    RejoinFacts {
        frames_before,
        frames_after,
    }
}
