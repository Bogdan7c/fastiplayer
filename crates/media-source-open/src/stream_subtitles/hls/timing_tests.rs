//! Арифметика времени HLS: разворот 33-битного счётчика и формулы привязки.

use std::time::Duration;

use subtitle_formats::MpegTsTimestamp90kHz;

use super::timing::{OriginMicros, cue_to_player_shift, media_to_player, unwrap_mpegts_near};

fn mpegts(ticks: u64) -> MpegTsTimestamp90kHz {
    MpegTsTimestamp90kHz::new(ticks).expect("ticks fit into 33 bits")
}

/// Секунд в одном обороте счётчика, как считает демуксер (2^33 / 90000).
fn wrap_duration_micros() -> i64 {
    i64::try_from(
        MpegTsTimestamp90kHz::MODULUS * 1_000_000 / MpegTsTimestamp90kHz::TICKS_PER_SECOND,
    )
    .expect("one wrap fits i64")
}

#[test]
fn value_without_rollover_is_returned_as_is() {
    // 10 с при точке отсчёта около 9,97 с: оборотов нет.
    let unwrapped = unwrap_mpegts_near(mpegts(900_000), Duration::from_millis(9_967));
    assert_eq!(unwrapped, Some(10_000_000));
}

#[test]
fn label_just_before_wrap_is_negative_when_video_origin_is_after_wrap() {
    // VTT-метка за 1 с до оборота, а демуксер видео уже начал эпоху заново (origin 0,5 с):
    // ближайшее значение - на секунду ДО нуля, а не 26 часов вперёд.
    let before_wrap = mpegts(MpegTsTimestamp90kHz::MODULUS - 90_000);
    let unwrapped = unwrap_mpegts_near(before_wrap, Duration::from_millis(500));
    assert_eq!(unwrapped, Some(-1_000_000));
}

#[test]
fn label_after_wrap_is_lifted_by_one_wrap_when_origin_is_before_wrap() {
    // Origin видео почти в конце оборота (до оборота), метка VTT уже после оборота (1 с).
    let reference =
        Duration::from_micros(u64::try_from(wrap_duration_micros()).expect("positive") - 500_000);
    let unwrapped = unwrap_mpegts_near(mpegts(90_000), reference);
    assert_eq!(unwrapped, Some(wrap_duration_micros() + 1_000_000));
}

#[test]
fn value_already_unwrapped_by_demuxer_keeps_its_wrap_count() {
    // Демуксер продолжил счёт за оборотом (origin = 1 оборот + 3 с): метка 4 с после оборота
    // должна лечь рядом с ним, а не вернуться к нулевому обороту.
    let reference =
        Duration::from_micros(u64::try_from(wrap_duration_micros()).expect("positive") + 3_000_000);
    let unwrapped = unwrap_mpegts_near(mpegts(360_000), reference);
    assert_eq!(unwrapped, Some(wrap_duration_micros() + 4_000_000));
}

#[test]
fn cue_shift_follows_the_demuxer_formula() {
    // Origin видео 9,9667 с, эпоха начинается на 0 с; MPEGTS = 10 с при LOCAL = 2 с:
    // реплика на 5 с файла -> media 13 с -> player 13 - 9,9667.
    let origin = OriginMicros {
        media_origin: 9_966_667,
        timeline_start: 0,
    };
    let shift = cue_to_player_shift(10_000_000, 2_000_000, origin).expect("shift");
    let cue_in_file = 5_000_000;
    assert_eq!(cue_in_file + shift, 13_000_000 - 9_966_667);
    assert_eq!(media_to_player(10_000_000, origin), Some(33_333));
}

#[test]
fn second_epoch_start_is_added_to_the_player_time() {
    let origin = OriginMicros {
        media_origin: 50_000_000,
        timeline_start: 1_000_000,
    };
    assert_eq!(media_to_player(51_000_000, origin), Some(2_000_000));
}
