//! Live: стабильная шкала `start` между перечитываниями и смена её поколения.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::fake::{
    AnchoredPending, FakeTransport, PLAYER_OFFSET_SECS, config, event_segments, index,
    ready_segment, start,
};
use crate::stream_subtitles::{SegmentDecode, StreamIndexRefresh};

/// Смещение «плеер = плейлист + N» у сегментов после перезакрепления шкалы.
const RE_ANCHORED_OFFSET_SECS: u64 = 5_000;

fn fast_refresh() -> StreamIndexRefresh {
    StreamIndexRefresh::After(Duration::from_millis(100))
}

#[test]
fn dvr_seek_after_sliding_reloads_uses_the_offset_learned_by_a_late_resolved_segment() {
    // Окно из 8 сегментов скользит: [0..8) -> [2..10) -> [4..12); `start` сегмента = key * 10 с
    // на всех снимках (контракт стабильной шкалы транспорта).
    let released = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&released);
    let transport = FakeTransport::new(
        |call| {
            let first = match call {
                0 => 0,
                1 => 2,
                _ => 4,
            };
            Ok(index(first, 8, fast_refresh()))
        },
        move |key| {
            Ok(SegmentDecode::Pending(Box::new(AnchoredPending {
                released: Arc::clone(&gate),
                key: key.value(),
            })))
        },
    );
    let mut live = config();
    // Вытеснения из списка ожидания в этом сценарии ожидаемы и не должны гасить дорожку.
    live.failure_threshold = std::num::NonZeroU32::new(50).expect("non-zero");
    // Плеер у живого края: 1000 + 110 с.
    let harness = start(Arc::clone(&transport), live, PLAYER_OFFSET_SECS + 110);

    // Ждём, пока плейлист доскользит до последнего снимка, и только потом «публикуем» привязку.
    let deadline = std::time::Instant::now() + super::fake::WAIT;
    while transport.index_loads() < 4 {
        assert!(std::time::Instant::now() < deadline, "playlist never slid");
        std::thread::sleep(Duration::from_millis(10));
    }
    released.store(true, Ordering::SeqCst);
    // Привязка выучена по ожидавшим сегментам (их playlist_start снят при скачивании).
    harness.collect_until(|messages| !event_segments(messages).is_empty());
    let fetched_before_seek = transport.fetched().len();

    // DVR назад: позиция в сегменте 4 (плейлист 45 с). Сегмент 4 хвостом не скачивался.
    harness
        .position
        .jump_to(Duration::from_secs(PLAYER_OFFSET_SECS + 45));
    let messages = harness.collect_until(|messages| event_segments(messages).contains(&4));
    let after_seek = transport.fetched()[fetched_before_seek..].to_vec();
    assert_eq!(
        after_seek.first(),
        Some(&4),
        "the segment holding the DVR position is requested first, got {after_seek:?}"
    );
    assert!(event_segments(&messages).contains(&4));
}

#[test]
fn reanchored_timeline_makes_the_feed_forget_the_stale_playlist_offset() {
    // Первый индекс [0..4): выучиваем смещение 1000 с. Затем плейлист «ушёл» дальше окна
    // перечитывания: транспорт закрепил шкалу заново (новое поколение), `start` новых
    // сегментов - 200..240 с без связи со старым смещением.
    let transport = FakeTransport::new(
        |call| {
            Ok(if call == 0 {
                index(0, 4, fast_refresh())
            } else {
                index(20, 4, fast_refresh())
            })
        },
        |key| {
            // Новая шкала: привязка уже на другом смещении, чем у сегментов первого индекса.
            let offset_secs = if key.value() >= 20 {
                RE_ANCHORED_OFFSET_SECS
            } else {
                PLAYER_OFFSET_SECS
            };
            let anchor_ms =
                u32::try_from((key.value() * 10 + offset_secs) * 1_000).expect("small test anchor");
            Ok(ready_segment(key.value(), Some(anchor_ms)))
        },
    );
    // Объявляем новое поколение заранее: оно относится ко второму и следующим индексам,
    // а первый индекс лента уже приняла под нулевым поколением.
    let harness = start(Arc::clone(&transport), config(), PLAYER_OFFSET_SECS + 35);
    harness.collect_until(|messages| event_segments(messages).contains(&3));
    transport.set_timeline_epoch(1);
    harness.collect_until(|messages| event_segments(messages).contains(&23));

    // Без забывания смещения (1000 с) окно по позиции 1035 с искало бы playlist 35 с и не нашло
    // сегменты 20..24 (200..240 с): они бы не скачались. Забытое смещение = хвост у края.
    let fetched = transport.fetched();
    assert!(
        fetched.contains(&23),
        "tail of the re-anchored playlist is fetched, got {fetched:?}"
    );
}
