// Fastiplayer: арифметика signed Track ticks, на которой держится вычитание CodecDelay из pts.

use crate::segment::{SignedTrackTicks, TrackTicks};

/// Вычитание уменьшает значение и может уйти в минус: так первый блок Opus с CodecDelay
/// 6.5 мс (6 тиков по 1 мс) получает pts −6, как велит RFC 9559 («CodecDelay MUST be
/// subtracted from each Block timestamp»). Upstream здесь прибавлял.
#[test]
fn checked_sub_unsigned_subtracts_and_may_go_negative() {
    let block_timestamp = SignedTrackTicks::from(0);
    let codec_delay = TrackTicks::from(6);

    let presentation = block_timestamp.checked_sub_unsigned(codec_delay);

    assert_eq!(presentation.map(|ticks| ticks.get()), Some(-6));
    assert_eq!(
        SignedTrackTicks::from(4_981)
            .checked_sub_unsigned(TrackTicks::from(6))
            .map(|ticks| ticks.get()),
        Some(4_975)
    );
}

/// Переполнение вниз не заворачивается, а честно даёт `None` (вызывающий код завершает поток).
#[test]
fn checked_sub_unsigned_reports_underflow() {
    let lowest = SignedTrackTicks::from(i64::MIN);

    assert_eq!(lowest.checked_sub_unsigned(TrackTicks::from(1)), None);
}

/// Сложение остаётся сложением: длительности кадров в laced-блоке сдвигают pts вперёд.
#[test]
fn checked_add_unsigned_still_adds() {
    assert_eq!(
        SignedTrackTicks::from(-6)
            .checked_add_unsigned(TrackTicks::from(20))
            .map(|ticks| ticks.get()),
        Some(14)
    );
}
