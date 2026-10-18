//! Закрепление скользящих снимков live-плейлиста на стабильной шкале.

use std::time::Duration;

use super::*;
use crate::subtitles::playlist::HlsSubtitleSegmentRef;

fn secs(value: u64) -> Duration {
    Duration::from_secs(value)
}

/// Снимок из сегментов по 2 с с заданными media sequence; `start` - локальный (от нуля).
fn window(first_sequence: u64, count: u64, discontinuity: u64) -> Vec<HlsSubtitleSegment> {
    (0..count)
        .map(|offset| HlsSubtitleSegment {
            reference: HlsSubtitleSegmentRef::for_tests(first_sequence + offset),
            media_sequence: first_sequence + offset,
            discontinuity_sequence: discontinuity,
            start: secs(offset * 2),
            duration: secs(2),
        })
        .collect()
}

fn starts(segments: &[HlsSubtitleSegment]) -> Vec<(u64, u64)> {
    segments
        .iter()
        .map(|segment| (segment.media_sequence, segment.start.as_secs()))
        .collect()
}

#[test]
fn sliding_reloads_keep_the_start_of_every_retained_segment() {
    let mut timeline = PlaylistTimeline::default();
    let mut first = window(10, 3, 0);
    let first_epoch = timeline.anchor(&mut first);
    assert_eq!(starts(&first), [(10, 0), (11, 2), (12, 4)]);

    // Окно уехало на два сегмента: локально сегмент 12 стал нулевым, на шкале он остался на 4 с.
    let mut second = window(12, 3, 0);
    let second_epoch = timeline.anchor(&mut second);
    assert_eq!(starts(&second), [(12, 4), (13, 6), (14, 8)]);

    let mut third = window(13, 3, 0);
    let third_epoch = timeline.anchor(&mut third);
    assert_eq!(starts(&third), [(13, 6), (14, 8), (15, 10)]);
    assert_eq!(first_epoch, second_epoch);
    assert_eq!(second_epoch, third_epoch);
}

#[test]
fn a_reload_without_overlap_continues_the_timeline_and_bumps_the_epoch() {
    let mut timeline = PlaylistTimeline::default();
    let mut first = window(10, 3, 0);
    let first_epoch = timeline.anchor(&mut first);

    // Окно перечитывания пропущено целиком: общих сегментов нет.
    let mut gap = window(40, 2, 0);
    let gap_epoch = timeline.anchor(&mut gap);

    // Шкала продолжается от конца прошлого снимка (6 с), поколение выросло.
    assert_eq!(starts(&gap), [(40, 6), (41, 8)]);
    assert!(
        gap_epoch > first_epoch,
        "learned offsets must be invalidated"
    );

    // Следующее скольжение после разрыва снова стабильно и поколение не меняет.
    let mut next = window(41, 2, 0);
    assert_eq!(timeline.anchor(&mut next), gap_epoch);
    assert_eq!(starts(&next), [(41, 8), (42, 10)]);
}

#[test]
fn same_media_sequence_in_another_discontinuity_epoch_is_not_an_overlap() {
    let mut timeline = PlaylistTimeline::default();
    let mut first = window(10, 2, 0);
    let first_epoch = timeline.anchor(&mut first);
    let mut other = window(10, 2, 5);
    assert!(timeline.anchor(&mut other) > first_epoch);
}

#[test]
fn empty_snapshot_changes_nothing() {
    let mut timeline = PlaylistTimeline::default();
    let mut first = window(10, 2, 0);
    let epoch = timeline.anchor(&mut first);
    assert_eq!(timeline.anchor(&mut []), epoch);
    let mut again = window(11, 2, 0);
    assert_eq!(timeline.anchor(&mut again), epoch);
    assert_eq!(starts(&again), [(11, 2), (12, 4)]);
}
