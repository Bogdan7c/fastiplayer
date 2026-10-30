//! Журнал прогона: счёт, монотонность позиции, приватность строки, ритм сводок.

use std::time::{Duration, Instant};

use std::num::NonZeroU64;

use player_core::{
    MediaInstanceId, MediaSummary, PlaybackResumeIntent, PlaybackState, PlayerError,
    PlayerErrorKind, PlayerEvent, SeekCommitInfo,
};

use super::*;
use crate::browser_session::playback::metered_audio::{AudioMeterReading, AudioOutputStatus};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// Экземпляр медиа с номером `number` (≥ 1).
fn instance(number: u64) -> Option<MediaInstanceId> {
    NonZeroU64::new(number).map(MediaInstanceId::from_non_zero)
}

fn journal_at(start: Instant) -> PlaybackJournal {
    PlaybackJournal::new(start, BROWSER_PLAYBACK_SUMMARY_INTERVAL)
}

/// Подтверждение перемотки, как его шлёт плеер.
fn seek_committed() -> PlayerEvent {
    PlayerEvent::SeekCommitted(SeekCommitInfo {
        target_position: ms(2_000),
        actual_position: ms(2_000),
        resume_intent: PlaybackResumeIntent::Play,
    })
}

/// Кадры, звук, загрузки и буферизации складываются в сводку.
#[test]
fn counters_accumulate_into_the_summary() {
    let mut journal = journal_at(Instant::now());
    journal.note_media_load();
    journal.note_presented_frames(4);
    journal.note_presented_frames(6);
    journal.note_taken_frames(9);
    journal.note_audio(AudioMeterReading {
        written: ms(1_500),
        played: ms(1_200),
        output: AudioOutputStatus::Simulated,
    });
    assert_eq!(
        journal.note_snapshot(instance(1), ms(100), PlaybackState::Playing),
        None
    );
    assert_eq!(
        journal.note_snapshot(instance(1), ms(150), PlaybackState::Buffering),
        None
    );
    assert_eq!(
        journal.note_snapshot(instance(1), ms(150), PlaybackState::Buffering),
        None
    );
    assert_eq!(
        journal.note_snapshot(instance(1), ms(200), PlaybackState::Playing),
        None
    );
    assert_eq!(
        journal.note_snapshot(instance(1), ms(220), PlaybackState::Buffering),
        None
    );

    let summary = journal.summary();

    assert_eq!(
        summary.counters,
        PlaybackCounters {
            presented_frames: 10,
            taken_frames: 9,
            position_regressions: 0,
            loads: 1,
            buffering_entries: 2,
        }
    );
    assert_eq!(summary.position, ms(220));
    assert_eq!(summary.audio.played, ms(1_200));
}

/// Позиция назад без перемотки — откат; дальше счёт идёт от новой позиции.
#[test]
fn backwards_position_without_seek_is_a_regression() {
    let mut journal = journal_at(Instant::now());
    journal.note_media_load();
    journal.note_snapshot(instance(1), ms(1_000), PlaybackState::Playing);
    journal.note_snapshot(instance(1), ms(2_000), PlaybackState::Playing);
    journal.note_snapshot(instance(1), ms(1_500), PlaybackState::Playing);
    journal.note_snapshot(instance(1), ms(1_600), PlaybackState::Playing);
    journal.note_snapshot(instance(1), ms(1_599), PlaybackState::Playing);

    assert_eq!(journal.summary().counters.position_regressions, 2);
}

/// Перемотка назад законна от запроса до подтверждения, а после — снова строгий счёт.
#[test]
fn backwards_position_after_seek_is_not_a_regression() {
    let mut journal = journal_at(Instant::now());
    journal.note_media_load();
    journal.note_snapshot(instance(1), ms(10_000), PlaybackState::Playing);
    journal.note_seek_requested();
    // Пока перемотка не подтверждена, позиция прыгает как угодно.
    journal.note_snapshot(instance(1), ms(10_020), PlaybackState::Seeking);
    journal.note_snapshot(instance(1), ms(2_000), PlaybackState::Seeking);
    journal.note_player_event(&seek_committed());
    journal.note_snapshot(instance(1), ms(2_000), PlaybackState::Playing);
    journal.note_snapshot(instance(1), ms(2_100), PlaybackState::Playing);
    assert_eq!(journal.summary().counters.position_regressions, 0);

    // После подтверждения откат снова считается.
    journal.note_snapshot(instance(1), ms(1_000), PlaybackState::Playing);
    assert_eq!(journal.summary().counters.position_regressions, 1);
}

/// Сорвавшаяся перемотка закрывает окно «назад можно».
#[test]
fn failed_seek_ends_the_seek_window() {
    let mut journal = journal_at(Instant::now());
    journal.note_snapshot(instance(1), ms(5_000), PlaybackState::Playing);
    journal.note_seek_requested();
    journal.note_player_event(&PlayerEvent::RecoverableError(PlayerError::new(
        PlayerErrorKind::SeekUnavailable,
        "no seek",
    )));
    journal.note_snapshot(instance(1), ms(5_100), PlaybackState::Playing);
    journal.note_snapshot(instance(1), ms(4_000), PlaybackState::Playing);

    assert_eq!(journal.summary().counters.position_regressions, 1);
}

/// Новый экземпляр медиа — новая шкала: позиция с нуля не считается откатом, а снимки
/// прежнего экземпляра, пришедшие после запроса загрузки, ещё сравниваются по-старому.
#[test]
fn new_media_instance_starts_a_new_timeline() {
    let mut journal = journal_at(Instant::now());
    journal.note_media_load();
    journal.note_snapshot(instance(1), ms(30_000), PlaybackState::Playing);
    journal.note_media_load();
    // Плеер ещё не установил новое медиа: снимок прежнего экземпляра.
    journal.note_snapshot(instance(1), ms(30_010), PlaybackState::Playing);
    journal.note_snapshot(instance(2), ms(0), PlaybackState::Playing);
    journal.note_snapshot(instance(2), ms(40), PlaybackState::Playing);

    let counters = journal.summary().counters;
    assert_eq!(counters.position_regressions, 0);
    assert_eq!(counters.loads, 2);

    // Внутри нового экземпляра откат снова считается.
    journal.note_snapshot(instance(2), ms(20), PlaybackState::Playing);
    assert_eq!(journal.summary().counters.position_regressions, 1);
}

/// Конец медиа отмечается ровно один раз на переход в Ended.
#[test]
fn reaching_ended_is_noticed_once() {
    let mut journal = journal_at(Instant::now());
    assert_eq!(
        journal.note_snapshot(instance(1), ms(900), PlaybackState::Playing),
        None
    );
    assert_eq!(
        journal.note_snapshot(instance(1), ms(1_000), PlaybackState::Ended),
        Some(JournalNotice::PlaybackEnded)
    );
    assert_eq!(
        journal.note_snapshot(instance(1), ms(1_000), PlaybackState::Ended),
        None
    );
}

/// Последний снимок в Ended ставит позицию на длительность контейнера (звук бывает
/// длиннее на долю кадра) — это не откат; после конца отсчёт начинается заново.
#[test]
fn final_clamp_to_duration_at_the_end_is_not_a_regression() {
    let mut journal = journal_at(Instant::now());
    journal.note_snapshot(instance(1), ms(1_023), PlaybackState::Draining);
    journal.note_snapshot(instance(1), ms(1_000), PlaybackState::Ended);
    journal.note_snapshot(instance(1), ms(0), PlaybackState::Playing);
    journal.note_snapshot(instance(1), ms(50), PlaybackState::Playing);
    assert_eq!(journal.summary().counters.position_regressions, 0);
    assert_eq!(journal.summary().position, ms(50));

    // Тот же шаг назад, но без конца медиа, — откат.
    journal.note_snapshot(instance(1), ms(10), PlaybackState::Draining);
    assert_eq!(journal.summary().counters.position_regressions, 1);
}

/// Строка — стабильные ключи `key=value` в фиксированном порядке.
#[test]
fn summary_line_has_stable_keys() {
    let summary = PlaybackSummary {
        counters: PlaybackCounters {
            presented_frames: 10,
            taken_frames: 10,
            position_regressions: 0,
            loads: 1,
            buffering_entries: 2,
        },
        audio: AudioMeterReading {
            written: ms(1_000),
            played: ms(987),
            output: AudioOutputStatus::Unavailable,
        },
        position: ms(1_234),
        state: PlaybackState::Ended,
    };

    assert_eq!(
        summary.to_string(),
        "presented_frames=10 taken_frames=10 audio_written_ms=1000 audio_played_ms=987 \
         audio_output=unavailable position_regressions=0 loads=1 buffering_entries=2 \
         position_ms=1234 state=ended"
    );
}

/// Приватность: заголовок и домен страницы, пришедшие в событии плеера, в строку не попадают.
#[test]
fn summary_line_never_contains_page_title_or_domain() {
    let secret_title = "Secret Episode Title";
    let secret_domain = "private-site.example";
    let mut journal = journal_at(Instant::now());
    journal.note_media_load();
    journal.note_player_event(&PlayerEvent::MediaOpened(MediaSummary {
        title: Some(secret_title.to_string()),
        source_label: format!("https://{secret_domain}/watch?v=1"),
        duration: Some(ms(60_000)),
    }));
    journal.note_snapshot(instance(1), ms(500), PlaybackState::Playing);

    let line = journal.summary().to_string();

    assert!(!line.contains(secret_title), "{line}");
    assert!(!line.contains(secret_domain), "{line}");
    assert!(!line.contains("Secret"), "{line}");
    assert!(
        line.contains("loads=1"),
        "the line is the real summary: {line}"
    );
}

/// Промежуточная сводка — не раньше интервала и не чаще раза за интервал.
#[test]
fn periodic_summary_follows_the_interval() {
    let start = Instant::now();
    let interval = Duration::from_secs(10);
    let mut journal = PlaybackJournal::new(start, interval);

    assert!(journal.take_due_summary(start + ms(9_999)).is_none());
    assert!(journal.take_due_summary(start + ms(10_000)).is_some());
    assert!(journal.take_due_summary(start + ms(15_000)).is_none());
    assert!(journal.take_due_summary(start + ms(19_999)).is_none());
    assert!(journal.take_due_summary(start + ms(20_000)).is_some());
}

/// Интервал по умолчанию — 10 с (политика журнала, см. константу).
#[test]
fn default_summary_interval_is_ten_seconds() {
    assert_eq!(BROWSER_PLAYBACK_SUMMARY_INTERVAL, Duration::from_secs(10));
}
