//! Тесты разбора и арифметики списка правок: формы `elst`, пересчёт шкал, границы.

use std::io::Cursor;
use std::num::NonZero;

use symphonia_core::io::MediaSourceStream;

use super::{PresentationShift, UnsupportedEditList, rescale_ticks_rounded};
use crate::atoms::elst::{ElstAtom, ElstEntry};
use crate::atoms::{AtomIterator, AtomType};

fn scale(value: u32) -> NonZero<u32> {
    NonZero::new(value).expect("тестовый timescale ненулевой")
}

fn entry(segment_duration: u64, media_time: i64) -> ElstEntry {
    ElstEntry {
        segment_duration,
        media_time,
        media_rate_int: 1,
        media_rate_frac: 0,
    }
}

/// Читает `elst` из байтов full-box-а: проверяет настоящий парсер, а не ручную сборку структур.
fn parse_elst(version: u8, entries: &[(u64, i64)]) -> ElstAtom {
    let mut payload = vec![version, 0, 0, 0];
    payload.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (segment_duration, media_time) in entries {
        if version == 1 {
            payload.extend_from_slice(&segment_duration.to_be_bytes());
            payload.extend_from_slice(&media_time.to_be_bytes());
        } else {
            payload.extend_from_slice(&(*segment_duration as u32).to_be_bytes());
            payload.extend_from_slice(&(*media_time as i32).to_be_bytes());
        }
        payload.extend_from_slice(&[0, 1, 0, 0]); // media_rate 1.0
    }
    let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(b"elst");
    bytes.extend(payload);
    let length = bytes.len() as u64;
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut iterator = AtomIterator::new(source, Some(length));
    let header = iterator
        .next_header()
        .ok()
        .expect("заголовок читается")
        .expect("заголовок есть");
    assert_eq!(header.atom_type(), AtomType::EditList);
    iterator
        .read_atom::<ElstAtom>()
        .ok()
        .expect("elst разбирается")
}

#[test]
fn elst_parser_keeps_empty_edit_media_time_minus_one_in_both_versions() {
    for version in [0u8, 1] {
        let atom = parse_elst(version, &[(10832, -1), (0, 0)]);
        assert_eq!(
            atom.entries,
            vec![entry(10832, -1), entry(0, 0)],
            "версия {version}"
        );
    }
}

#[test]
fn ffmpeg_shape_leading_empty_edit_becomes_dwell_in_media_ticks() {
    // movie 1000 Hz -> media 48000 Hz: 226 мс = 10848 тиков.
    let shift = PresentationShift::try_from_entries(
        &[entry(226, -1), entry(0, 0)],
        scale(1000),
        scale(48_000),
    )
    .expect("форма ffmpeg поддержана");
    assert_eq!(shift.dwell_ticks(), 10_848);
    assert_eq!(shift.media_start_ticks(), 0);
    assert_eq!(shift.presentation_ticks(0), Some(10_848));
    assert_eq!(shift.presentation_ticks(1024), Some(11_872));
}

#[test]
fn several_leading_empty_edits_are_summed() {
    let shift = PresentationShift::try_from_entries(
        &[entry(100, -1), entry(50, -1), entry(0, 0)],
        scale(1000),
        scale(1000),
    )
    .expect("несколько пустых правок поддержаны");
    assert_eq!(shift.dwell_ticks(), 150);
}

#[test]
fn first_normal_edit_media_time_moves_presentation_start_back() {
    // Видео с B-frame: media-время 1328 соответствует началу презентации.
    let shift =
        PresentationShift::try_from_entries(&[entry(1_000_000, 1328)], scale(1000), scale(16_000))
            .expect("одна обычная правка поддержана");
    assert_eq!(shift.media_start_ticks(), 1328);
    assert_eq!(shift.presentation_ticks(1328), Some(0));
    assert_eq!(shift.presentation_ticks(0), Some(-1328));
}

#[test]
fn empty_edit_followed_by_media_time_combines_both_shifts() {
    let shift = PresentationShift::try_from_entries(
        &[entry(500, -1), entry(0, 1024)],
        scale(1000),
        scale(1000),
    )
    .expect("dwell плюс media_time поддержаны");
    assert_eq!(shift.presentation_ticks(1024), Some(500));
}

#[test]
fn unsupported_shapes_are_rejected_with_reason() {
    let two_normal = [entry(10, 0), entry(10, 5000)];
    let mut fast = entry(10, 0);
    fast.media_rate_int = 2;
    let negative_media_time = [entry(10, -5)];
    for entries in [&two_normal[..], &[fast][..], &negative_media_time[..]] {
        assert_eq!(
            PresentationShift::try_from_entries(entries, scale(1000), scale(1000)),
            Err(UnsupportedEditList::UnsupportedShape),
            "{entries:?}"
        );
    }
}

#[test]
fn lenient_wrapper_ignores_unsupported_list_without_shifting() {
    let atom = ElstAtom {
        entries: vec![entry(10, 0), entry(10, 5000)],
    };
    let shift = PresentationShift::from_track_edit_list(Some(&atom), scale(1000), scale(48_000));
    assert!(shift.is_identity());
    assert_eq!(shift.presentation_ticks(777), Some(777));
}

#[test]
fn missing_edit_list_is_identity() {
    assert!(
        PresentationShift::from_track_edit_list(None, scale(1000), scale(48_000)).is_identity()
    );
}

#[test]
fn dwell_overflow_is_a_typed_error_not_a_panic() {
    assert_eq!(
        PresentationShift::try_from_entries(
            &[entry(u64::MAX, -1), entry(1, -1)],
            scale(1000),
            scale(1000)
        ),
        Err(UnsupportedEditList::ArithmeticOverflow)
    );
    // Результат пересчёта не помещается в i64-таймстемп.
    assert_eq!(
        PresentationShift::try_from_entries(&[entry(u64::MAX / 2 + 1, -1)], scale(1), scale(1)),
        Err(UnsupportedEditList::ArithmeticOverflow)
    );
}

#[test]
fn rescale_rounds_half_up_and_rejects_overflow() {
    assert_eq!(
        rescale_ticks_rounded(1, scale(1000), scale(44_100)),
        Some(44)
    );
    assert_eq!(
        rescale_ticks_rounded(5, scale(1000), scale(44_100)),
        Some(221)
    );
    assert_eq!(rescale_ticks_rounded(7, scale(7), scale(7)), Some(7));
    assert_eq!(rescale_ticks_rounded(u64::MAX, scale(1), scale(2)), None);
}

#[test]
fn seek_target_maps_back_to_media_time_and_saturates_inside_dwell() {
    let shift = PresentationShift::try_from_entries(
        &[entry(226, -1), entry(0, 1024)],
        scale(1000),
        scale(48_000),
    )
    .expect("поддержано");
    // dwell 10848, M 1024: presentation 10848 + 5000 <-> media 6024.
    assert_eq!(
        shift.media_ticks_for_presentation(10_848 + 5_000),
        5_000 + 1_024
    );
    // Внутри dwell цель насыщается на первом sample.
    assert_eq!(shift.media_ticks_for_presentation(0), 0);
    let dwell_only =
        PresentationShift::try_from_entries(&[entry(226, -1)], scale(1000), scale(48_000))
            .expect("поддержано");
    assert_eq!(dwell_only.media_ticks_for_presentation(100), 0);
}

#[test]
fn duration_is_presentation_end_and_net_shift_follows_target_timescale() {
    let shift = PresentationShift::try_from_entries(
        &[entry(226, -1), entry(0, 960)],
        scale(1000),
        scale(48_000),
    )
    .expect("поддержано");
    // 10848 dwell - 960 M: длительность 48000 media-тиков -> 48000 + 10848 - 960.
    assert_eq!(
        shift.presentation_duration_ticks(48_000, scale(48_000)),
        Some(57_888)
    );
    // В шкале 24000 сдвиг ровно вдвое меньше: (10848 - 960) / 2.
    assert_eq!(shift.net_shift_in(scale(24_000)), Some(4_944));
}
