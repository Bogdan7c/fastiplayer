//! Планировщик: порядок и границы окна, исключения, хвост live.

use std::collections::HashSet;
use std::time::Duration;

use super::schedule::{PlaylistOffset, WindowQuery, next_segment};
use crate::stream_subtitles::transport::{
    StreamIndexRefresh, StreamSegment, StreamSegmentIndex, StreamSegmentKey, StreamSegmentRef,
};

/// Шесть сегментов по 10 с: ключи 0..=5, старты 0, 10, ..., 50.
fn index() -> StreamSegmentIndex {
    StreamSegmentIndex {
        segments: (0..6)
            .map(|number| StreamSegment {
                key: StreamSegmentKey::new(number),
                reference: StreamSegmentRef::new(()),
                start: Duration::from_secs(number * 10),
                duration: Duration::from_secs(10),
            })
            .collect(),
        refresh: StreamIndexRefresh::Never,
    }
}

/// Последовательно вынимает сегменты, пока планировщик что-то предлагает.
fn drain_order(
    index: &StreamSegmentIndex,
    position_secs: u64,
    ahead_secs: u64,
    behind_secs: u64,
    offset: PlaylistOffset,
) -> Vec<u64> {
    let mut taken: HashSet<StreamSegmentKey> = HashSet::new();
    let mut order = Vec::new();
    loop {
        let query = WindowQuery {
            index,
            position: Duration::from_secs(position_secs),
            ahead: Duration::from_secs(ahead_secs),
            behind: Duration::from_secs(behind_secs),
            offset,
        };
        let Some(segment) = next_segment(&query, |key| taken.contains(&key)) else {
            return order;
        };
        order.push(segment.key.value());
        taken.insert(segment.key);
    }
}

#[test]
fn window_goes_forward_from_position_then_backwards_nearest_first() {
    // Позиция 25 с, окно [5, 45]: вперёд 2 (содержит позицию), 3, 4; назад 1, 0.
    let order = drain_order(&index(), 25, 20, 20, PlaylistOffset::Identity);
    assert_eq!(order, vec![2, 3, 4, 1, 0]);
}

#[test]
fn segments_outside_the_window_are_never_requested() {
    let order = drain_order(&index(), 25, 10, 5, PlaylistOffset::Identity);
    // Окно [20, 35]: сегменты 2 и 3; сегмент 1 (10-20) заканчивается ровно на границе окна.
    assert_eq!(order, vec![2, 3]);
}

#[test]
fn learned_offset_moves_the_window_to_the_playlist_time() {
    // Плеер идёт на 125 с, плейлист на 100 с «впереди» плеера... смещение +100 с:
    // плеер = плейлист + 100, значит позиция 125 с = 25 с плейлиста.
    let offset = PlaylistOffset::Learned(100_000_000);
    let order = drain_order(&index(), 125, 20, 20, offset);
    assert_eq!(order, vec![2, 3, 4, 1, 0]);
}

#[test]
fn unknown_live_offset_starts_from_the_newest_segments_of_the_tail() {
    // Хвост = ahead + behind = 20 с до конца плейлиста (60 с): сегменты 4 и 5, новые первыми.
    let order = drain_order(&index(), 0, 15, 5, PlaylistOffset::Unknown);
    assert_eq!(order, vec![5, 4]);
}

#[test]
fn excluded_segments_are_skipped_without_losing_the_rest() {
    let index = index();
    let excluded: HashSet<StreamSegmentKey> = [StreamSegmentKey::new(2)].into_iter().collect();
    let query = WindowQuery {
        index: &index,
        position: Duration::from_secs(25),
        ahead: Duration::from_secs(20),
        behind: Duration::from_secs(0),
        offset: PlaylistOffset::Identity,
    };
    let chosen = next_segment(&query, |key| excluded.contains(&key)).expect("segment 3 remains");
    assert_eq!(chosen.key.value(), 3);
}
