use std::io::Cursor;

use symphonia_core::io::MediaSourceStream;

use super::TrunAtom;
use crate::atoms::{AtomError, AtomIterator};

/// Создаёт run с интересующими sync-seek полями.
fn run(
    sample_count: u32,
    flags: u32,
    first_sample_flags: Option<u32>,
    sample_flags: Vec<u32>,
) -> TrunAtom {
    TrunAtom {
        flags,
        data_offset: None,
        sample_count,
        first_sample_flags,
        sample_duration: Vec::new(),
        sample_size: Vec::new(),
        sample_flags,
        sample_composition_time_offset: Vec::new(),
        total_sample_size: 0,
        total_sample_duration: 0,
    }
}

/// Парсит synthetic `trun` через production atom reader.
fn parse_trun(payload: &[u8]) -> Result<TrunAtom, AtomError> {
    let size = u32::try_from(payload.len() + 8).expect("test trun fits u32");
    let mut bytes = Vec::with_capacity(payload.len() + 8);
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(b"trun");
    bytes.extend_from_slice(payload);

    let source_len = bytes.len() as u64;
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut iterator = AtomIterator::new(source, Some(source_len));
    iterator.next_header()?.expect("trun header");
    iterator.read_atom::<TrunAtom>()
}

#[test]
fn effective_flags_follow_iso_precedence() {
    const SAMPLE_FLAGS_PRESENT: u32 = 0x400;
    let per_sample = run(2, SAMPLE_FLAGS_PRESENT, None, vec![11, 12]);
    assert_eq!(per_sample.effective_sample_flags(0, 99), 11);
    assert_eq!(per_sample.effective_sample_flags(1, 99), 12);

    let first_override = run(2, 0, Some(21), Vec::new());
    assert_eq!(first_override.effective_sample_flags(0, 99), 21);
    assert_eq!(first_override.effective_sample_flags(1, 99), 99);

    let defaults_only = run(2, 0, None, Vec::new());
    assert_eq!(defaults_only.effective_sample_flags(0, 99), 99);
    assert_eq!(defaults_only.effective_sample_flags(1, 99), 99);
}

#[test]
fn sync_status_rejects_non_sync_dependent_and_reserved_flags() {
    let defaults_only = run(1, 0, None, Vec::new());

    assert!(!defaults_only.is_proven_sync_sample(0, 0));
    assert!(!defaults_only.is_proven_sync_sample(0, 0x0001_0000));
    assert!(!defaults_only.is_proven_sync_sample(0, 0x0100_0000));
    assert!(!defaults_only.is_proven_sync_sample(0, 0x0300_0000));
    assert!(defaults_only.is_proven_sync_sample(0, 0x0200_0000));
    assert!(!defaults_only.is_proven_sync_sample(0, 0x0201_0000));
}

#[test]
fn sync_search_rolls_back_to_nearest_proven_sample() {
    const SAMPLE_FLAGS_PRESENT: u32 = 0x400;
    const PROVEN_SYNC: u32 = 0x0200_0000;
    const NON_SYNC: u32 = 0x0101_0000;
    let samples = run(
        4,
        SAMPLE_FLAGS_PRESENT,
        None,
        vec![PROVEN_SYNC, NON_SYNC, PROVEN_SYNC, NON_SYNC],
    );

    assert_eq!(samples.sync_sample_at_or_before(3, NON_SYNC), Some(2));
    assert_eq!(samples.sync_sample_at_or_before(1, NON_SYNC), Some(0));

    let unknown = run(2, SAMPLE_FLAGS_PRESENT, None, vec![0; 2]);
    assert_eq!(unknown.sync_sample_at_or_before(1, 0), None);

    let reserved = run(2, SAMPLE_FLAGS_PRESENT, None, vec![0x0300_0000; 2]);
    assert_eq!(reserved.sync_sample_at_or_before(1, 0), None);
}

#[test]
fn first_sample_flags_do_not_change_default_duration_or_size() {
    let first_override = run(3, 0, Some(0x0003_0000), Vec::new());

    assert_eq!(first_override.total_duration(10), 30);
    assert_eq!(first_override.sample_timing(1, 10), (10, 10));
    assert_eq!(first_override.total_size(7), 21);
    assert_eq!(first_override.sample_offset(1, 7), (7, 7));
}

#[test]
fn parses_positive_and_signed_composition_offsets() {
    let mut unsigned_payload = vec![0, 0, 8, 0];
    unsigned_payload.extend_from_slice(&2_u32.to_be_bytes());
    unsigned_payload.extend_from_slice(&7_u32.to_be_bytes());
    unsigned_payload.extend_from_slice(&9_u32.to_be_bytes());
    let unsigned = match parse_trun(&unsigned_payload) {
        Ok(trun) => trun,
        Err(_) => panic!("valid version 0 trun должен парситься"),
    };
    assert_eq!(unsigned.sample_composition_offset(0), 7);
    assert_eq!(unsigned.sample_composition_offset(1), 9);

    let mut signed_payload = vec![1, 0, 8, 0];
    signed_payload.extend_from_slice(&1_u32.to_be_bytes());
    signed_payload.extend_from_slice(&(-4_i32).to_be_bytes());
    let signed = match parse_trun(&signed_payload) {
        Ok(trun) => trun,
        Err(_) => panic!("valid version 1 trun должен парситься"),
    };
    assert_eq!(signed.sample_composition_offset(0), -4);
}
