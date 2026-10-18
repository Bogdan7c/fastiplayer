//! Live: перечитывание индекса, новые сегменты, выученное смещение и DVR-перемотка.

use std::sync::Arc;
use std::time::Duration;

use super::fake::{FakeTransport, config, event_segments, index, ready_segment, start};
use crate::stream_subtitles::{StreamIndexRefresh, StreamSubtitleMessage};

/// Смещение «плеер = плейлист + 1000 с», которое транспорт сообщает при привязке.
const PLAYER_OFFSET_SECS: u32 = 1_000;

fn live_refresh() -> StreamIndexRefresh {
    StreamIndexRefresh::After(Duration::from_millis(150))
}

fn aligned_segment(key: u64) -> SegmentResult {
    // Начало сегмента на шкале плеера = старт в плейлисте + смещение.
    let playlist_start_secs = u32::try_from(key).expect("small key") * 10;
    Ok(ready_segment(
        key,
        Some((playlist_start_secs + PLAYER_OFFSET_SECS) * 1_000),
    ))
}

type SegmentResult =
    Result<crate::stream_subtitles::SegmentDecode, crate::stream_subtitles::StreamSubtitleError>;

#[test]
fn live_starts_at_the_edge_learns_the_offset_and_picks_up_new_segments() {
    // Первая загрузка: сегменты 0 и 1; со второй - 2 и 3 (плейлист «вырос»).
    let transport = FakeTransport::new(
        |call| {
            Ok(if call == 0 {
                index(0, 2, live_refresh())
            } else {
                index(0, 4, live_refresh())
            })
        },
        |key| aligned_segment(key.value()),
    );
    // Позиция плеера 1015 с = 15 с плейлиста: окно [5, 35] после привязки.
    let harness = start(Arc::clone(&transport), config(), 1_015);

    let messages = harness.collect_until(|messages| {
        let segments = event_segments(messages);
        segments.contains(&2) && segments.contains(&3)
    });
    let fetched = transport.fetched();
    // Первым скачан самый новый сегмент у живого края, он же выучил смещение.
    assert_eq!(fetched.first(), Some(&1));
    // Новые сегменты из позднего снимка плейлиста дошли до приложения.
    assert!(event_segments(&messages).contains(&3));
    // Реплики пришли именно на шкале плеера, как отдал транспорт (без пересчёта лентой).
    let StreamSubtitleMessage::Events(first) = messages
        .iter()
        .find(|m| matches!(m, StreamSubtitleMessage::Events(_)))
        .expect("events")
    else {
        unreachable!("filtered above");
    };
    assert_eq!(first.segment.value(), 1);
}

#[test]
fn dvr_seek_back_uses_the_learned_offset_to_fetch_old_segments() {
    let transport = FakeTransport::new(
        |_| Ok(index(0, 6, live_refresh())),
        |key| aligned_segment(key.value()),
    );
    // Старт у края: плеер 1055 с = 55 с плейлиста; окно [45, 75], хвост = 30 с.
    let harness = start(Arc::clone(&transport), config(), 1_055);
    harness.collect_until(|messages| event_segments(messages).contains(&5));
    harness.collect_until_quiet_after_window();

    // Перемотка на 1012 с (= 12 с плейлиста): окно [2, 32] -> сегменты 1, 2, 3 (+0 по behind).
    harness.position.jump_to(Duration::from_secs(1_012));
    let messages = harness.collect_until(|messages| {
        let segments = event_segments(messages);
        segments.contains(&1) && segments.contains(&2) && segments.contains(&3)
    });
    let first_after_jump = event_segments(&messages)[0];
    assert_eq!(
        first_after_jump, 1,
        "window starts at the segment holding the position"
    );
    assert!(transport.fetched().contains(&3));
}

impl super::fake::Harness {
    /// Даёт ленте закончить первое окно: дальше сообщения считаем только после прыжка.
    fn collect_until_quiet_after_window(&self) {
        let _ = self.messages_within(Duration::from_millis(500));
    }
}
