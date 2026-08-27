use super::{HdsBootstrapError, HdsBootstrapLimits, parse_bootstrap};
use std::num::NonZeroUsize;

/// Проверяет минимальную VOD timeline из abst/asrt/afrt без сетевого fixture.
#[test]
fn parses_vod_fragment_timeline_and_segment_mapping() {
    let bootstrap = abst_box();
    let limits = HdsBootstrapLimits {
        maximum_bytes: non_zero(4096),
        maximum_boxes: non_zero(8),
        maximum_fragments: non_zero(16),
        maximum_string_bytes: non_zero(64),
    };

    let timeline = parse_bootstrap(&bootstrap, "video", limits).expect("valid HDS bootstrap");

    assert!(!timeline.live());
    assert_eq!(timeline.timescale(), 1_000);
    assert_eq!(timeline.fragments().len(), 2);
    assert_eq!(timeline.fragments()[0].segment(), 1);
    assert_eq!(timeline.fragments()[0].fragment(), 1);
    assert_eq!(timeline.fragments()[1].segment(), 1);
    assert_eq!(timeline.fragments()[1].fragment(), 2);
    assert_eq!(timeline.fragments()[1].timestamp(), 1_000);
}

/// Проверяет, что malformed binary не превращается в пустую timeline.
#[test]
fn rejects_truncated_bootstrap() {
    let limits = HdsBootstrapLimits {
        maximum_bytes: non_zero(64),
        maximum_boxes: non_zero(8),
        maximum_fragments: non_zero(16),
        maximum_string_bytes: non_zero(64),
    };

    assert!(parse_bootstrap(&[0, 0, 0], "video", limits).is_err());
}

/// Первый advertised fragment задаётся `afrt`, а не неявной единицей.
#[test]
fn maps_non_one_first_fragment_from_afrt_to_asrt() {
    let bootstrap = abst_box_with(asrt_box(4), afrt_box(9, 13, 5_000, 1_000, true), 1_000, 0);

    let timeline =
        parse_bootstrap(&bootstrap, "video", limits()).expect("valid shifted HDS timeline");

    assert_eq!(timeline.fragments().len(), 4);
    assert_eq!(timeline.fragments()[0].segment(), 1);
    assert_eq!(timeline.fragments()[0].fragment(), 9);
    assert_eq!(timeline.fragments()[0].timestamp(), 5_000);
    assert_eq!(timeline.fragments()[3].fragment(), 12);
}

/// Последний normal `afrt` run legal: segment table задаёт точный tail bound.
#[test]
fn expands_vod_without_terminal_discontinuity() {
    let bootstrap = abst_box_with(asrt_box(2), afrt_box(1, 3, 0, 1_000, false), 1_000, 0);

    let timeline =
        parse_bootstrap(&bootstrap, "video", limits()).expect("normal tail is supported");

    assert_eq!(timeline.fragments().len(), 2);
    assert_eq!(timeline.fragments()[1].fragment(), 2);
    assert_eq!(timeline.fragments()[1].timestamp(), 1_000);
}

/// Реальный packager может кодировать `END_OF_PRESENTATION` нулевым fragment ID.
#[test]
fn zero_id_terminal_marker_does_not_truncate_last_media_run() {
    let mut afrt_payload = vec![0, 0, 0, 0];
    afrt_payload.extend_from_slice(&1_000_u32.to_be_bytes());
    afrt_payload.push(0);
    afrt_payload.extend_from_slice(&3_u32.to_be_bytes());
    append_fragment_run(&mut afrt_payload, 1, 0, 1_000, None);
    append_fragment_run(&mut afrt_payload, 2, 1_000, 500, None);
    append_fragment_run(&mut afrt_payload, 0, 0, 0, Some(0));
    let bootstrap = abst_box_with(asrt_box(2), iso_box(b"afrt", &afrt_payload), 1_000, 0);

    let timeline = parse_bootstrap(&bootstrap, "video", limits())
        .expect("zero-id terminal marker is outside the media namespace");

    assert_eq!(timeline.fragments().len(), 2);
    assert_eq!(timeline.fragments()[1].fragment(), 2);
    assert_eq!(timeline.fragments()[1].timestamp(), 1_000);
    assert_eq!(timeline.fragments()[1].duration(), 500);
}

/// End marker не может маскироваться под fragment из advertised media range.
#[test]
fn rejects_terminal_marker_inside_advertised_media_range() {
    let bootstrap = abst_box_with(asrt_box(2), afrt_box(1, 2, 0, 1_000, true), 1_000, 0);

    assert_eq!(
        parse_bootstrap(&bootstrap, "video", limits()),
        Err(HdsBootstrapError::Unsupported)
    );
}

/// Недоверенный table count отклоняется до `Vec::with_capacity`.
#[test]
fn rejects_table_count_above_fragment_budget_before_allocation() {
    let mut oversized_asrt_payload = vec![0, 0, 0, 0, 0];
    oversized_asrt_payload.extend_from_slice(&u32::MAX.to_be_bytes());
    let bootstrap = abst_box_with(
        iso_box(b"asrt", &oversized_asrt_payload),
        afrt_box(1, 3, 0, 1_000, true),
        1_000,
        0,
    );

    assert_eq!(
        parse_bootstrap(&bootstrap, "video", limits()),
        Err(HdsBootstrapError::LimitExceeded)
    );
}

/// `afrt` clock обязан совпадать с enclosing `abst` clock.
#[test]
fn rejects_mismatched_fragment_timescale() {
    let bootstrap = abst_box_with(asrt_box(2), afrt_box(1, 3, 0, 90_000, true), 1_000, 0);

    assert_eq!(
        parse_bootstrap(&bootstrap, "video", limits()),
        Err(HdsBootstrapError::Malformed)
    );
}

/// Static VOD parser не принимает partial bootstrap update.
#[test]
fn rejects_bootstrap_update_control_bit() {
    let bootstrap = abst_box_with(asrt_box(2), afrt_box(1, 3, 0, 1_000, true), 1_000, 0x10);

    assert_eq!(
        parse_bootstrap(&bootstrap, "video", limits()),
        Err(HdsBootstrapError::Unsupported)
    );
}

/// Неподдержанная discontinuity не должна молча обрезать VOD.
#[test]
fn rejects_non_terminal_discontinuity() {
    let mut afrt_payload = vec![0, 0, 0, 0];
    afrt_payload.extend_from_slice(&1_000_u32.to_be_bytes());
    afrt_payload.push(0);
    afrt_payload.extend_from_slice(&3_u32.to_be_bytes());
    append_fragment_run(&mut afrt_payload, 1, 0, 1_000, None);
    append_fragment_run(&mut afrt_payload, 2, 1_000, 0, Some(2));
    append_fragment_run(&mut afrt_payload, 3, 2_000, 1_000, None);
    let bootstrap = abst_box_with(asrt_box(3), iso_box(b"afrt", &afrt_payload), 1_000, 0);

    assert_eq!(
        parse_bootstrap(&bootstrap, "video", limits()),
        Err(HdsBootstrapError::Unsupported)
    );
}

/// Строит ISO box с 32-bit size.
fn iso_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len() + 8).expect("fixture box fits u32");
    let mut bytes = Vec::with_capacity(payload.len() + 8);
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(payload);
    bytes
}

/// Строит минимальный `asrt`: два fragment-а на один segment.
fn asrt_box(fragments_per_segment: u32) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0, 0];
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&fragments_per_segment.to_be_bytes());
    iso_box(b"asrt", &payload)
}

/// Строит `afrt` с одним media run и optional terminal marker.
fn afrt_box(
    first_fragment: u32,
    terminal_fragment: u32,
    first_timestamp: u64,
    timescale: u32,
    terminal_marker: bool,
) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&timescale.to_be_bytes());
    payload.push(0);
    let run_count = if terminal_marker { 2_u32 } else { 1_u32 };
    payload.extend_from_slice(&run_count.to_be_bytes());
    append_fragment_run(&mut payload, first_fragment, first_timestamp, 1_000, None);
    if terminal_marker {
        append_fragment_run(&mut payload, terminal_fragment, 0, 0, Some(0));
    }
    iso_box(b"afrt", &payload)
}

/// Добавляет одну wire-level FRAGMENTRUNENTRY.
fn append_fragment_run(
    payload: &mut Vec<u8>,
    first_fragment: u32,
    first_timestamp: u64,
    duration: u32,
    discontinuity: Option<u8>,
) {
    payload.extend_from_slice(&first_fragment.to_be_bytes());
    payload.extend_from_slice(&first_timestamp.to_be_bytes());
    payload.extend_from_slice(&duration.to_be_bytes());
    if let Some(indicator) = discontinuity {
        payload.push(indicator);
    }
}

/// Строит полный `abst` с inline embedded tables.
fn abst_box() -> Vec<u8> {
    abst_box_with(asrt_box(2), afrt_box(1, 3, 0, 1_000, true), 1_000, 0)
}

/// Строит `abst` с caller-owned tables/clock/control byte.
fn abst_box_with(asrt: Vec<u8>, afrt: Vec<u8>, timescale: u32, profile_live_update: u8) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.push(profile_live_update);
    payload.extend_from_slice(&timescale.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(b"fixture\0");
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(b"\0\0");
    payload.push(1);
    payload.extend_from_slice(&asrt);
    payload.push(1);
    payload.extend_from_slice(&afrt);
    iso_box(b"abst", &payload)
}

/// Единый bounded policy для bootstrap unit tests.
fn limits() -> HdsBootstrapLimits {
    HdsBootstrapLimits {
        maximum_bytes: non_zero(4096),
        maximum_boxes: non_zero(8),
        maximum_fragments: non_zero(16),
        maximum_string_bytes: non_zero(64),
    }
}

/// Создаёт NonZeroUsize для bounded fixture policy.
fn non_zero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("positive test bound")
}
