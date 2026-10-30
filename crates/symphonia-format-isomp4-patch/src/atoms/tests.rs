use std::io::Cursor;

use symphonia_core::io::MediaSourceStream;

use super::decoder_configuration_tests::{
    atom_bytes, hevc_record_with_sei, parsed, visual_sample_entry,
};
use super::{AtomError, AtomIterator, SidxAtom, StsdAtom};
use crate::atoms::sidx::ReferenceType;
use symphonia_core::codecs::video::well_known::CODEC_ID_HEVC;

/// Создаёт iterator с известной физической длиной seekable test source.
fn iterator(bytes: Vec<u8>) -> AtomIterator<MediaSourceStream<'static>> {
    let source_len = bytes.len() as u64;
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    AtomIterator::new(source, Some(source_len))
}

/// Создаёт iterator неизвестной длины, как у streaming/progressive source без Content-Length.
fn streaming_iterator(bytes: Vec<u8>) -> AtomIterator<MediaSourceStream<'static>> {
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    AtomIterator::new(source, None)
}

#[test]
fn clean_box_boundary_eof_is_end_of_iteration() {
    let mut iterator = iterator(Vec::new());

    assert!(matches!(iterator.next_header(), Ok(None)));
}

#[test]
fn partial_top_level_header_is_structural_error() {
    let mut iterator = iterator(vec![0, 0, 0, 8]);

    assert!(matches!(
        iterator.next_header(),
        Err(AtomError::UnexpectedEndOfAtom)
    ));
}

#[test]
fn declared_box_larger_than_source_is_structural_error() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&16_u32.to_be_bytes());
    bytes.extend_from_slice(b"free");
    bytes.extend_from_slice(&[0; 4]);
    let mut iterator = iterator(bytes);

    assert!(matches!(
        iterator.next_header(),
        Err(AtomError::UnexpectedEndOfAtom)
    ));
}

#[test]
fn truncated_skipped_box_on_unknown_length_source_is_structural_error() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&16_u32.to_be_bytes());
    bytes.extend_from_slice(b"free");
    bytes.extend_from_slice(&[0; 4]);
    let mut iterator = streaming_iterator(bytes);

    assert!(matches!(iterator.next_header(), Ok(Some(_))));
    assert!(matches!(
        iterator.next_header(),
        Err(AtomError::UnexpectedEndOfAtom)
    ));
}

#[test]
fn indexed_top_level_seek_discards_old_pending_atom() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&8_u32.to_be_bytes());
    bytes.extend_from_slice(b"free");
    bytes.extend_from_slice(&8_u32.to_be_bytes());
    bytes.extend_from_slice(b"skip");
    let mut iterator = iterator(bytes);

    assert!(matches!(iterator.next_header(), Ok(Some(_))));
    assert!(iterator.seek_top_level(8).is_ok());

    let header = match iterator.next_header() {
        Ok(Some(header)) => header,
        _ => panic!("second atom header should be readable"),
    };
    assert_eq!(header.pos(), 8);
}

#[test]
fn sidx_parser_preserves_direct_sap_evidence() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&44_u32.to_be_bytes());
    bytes.extend_from_slice(b"sidx");
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&7_u32.to_be_bytes());
    bytes.extend_from_slice(&1_000_u32.to_be_bytes());
    bytes.extend_from_slice(&5_000_u32.to_be_bytes());
    bytes.extend_from_slice(&32_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&50_u32.to_be_bytes());
    bytes.extend_from_slice(&10_000_u32.to_be_bytes());
    bytes.extend_from_slice(&0x9000_0007_u32.to_be_bytes());
    let mut iterator = iterator(bytes);

    assert!(matches!(iterator.next_header(), Ok(Some(_))));
    let sidx = match iterator.read_atom::<SidxAtom>() {
        Ok(sidx) => sidx,
        Err(_) => panic!("direct sidx should parse"),
    };
    let reference = &sidx.references[0];
    assert!(matches!(reference.reference_type, ReferenceType::Media));
    assert!(reference.starts_with_sap);
    assert_eq!(reference.sap_type, 1);
    assert_eq!(reference.sap_delta_time, 7);
    assert_eq!(sidx.first_offset, 76);
}

/// Хвост короче заголовка атома (8 байт): длины 1..=7.
const SHORT_TAIL_LIMIT: usize = 8;

/// Собирает `stsd` с одним `hvc1`, у которого после `hvcC` лежит `tail` (внутри размера `hvc1`).
fn read_hvc1_stsd_with_tail(tail: &[u8]) -> super::Result<StsdAtom> {
    let record = hevc_record_with_sei(16);
    let mut children = atom_bytes(*b"hvcC", &record);
    children.extend_from_slice(tail);
    let entry = atom_bytes(*b"hvc1", &visual_sample_entry(&children));
    let mut stsd_payload = vec![0; 4];
    stsd_payload.extend_from_slice(&1_u32.to_be_bytes());
    stsd_payload.extend_from_slice(&entry);
    let bytes = atom_bytes(*b"stsd", &stsd_payload);
    let mut iterator = iterator(bytes);
    assert!(matches!(iterator.next_header(), Ok(Some(_))));
    iterator.read_atom::<StsdAtom>()
}

#[test]
fn quicktime_zero_terminator_after_sample_entry_children_is_skipped() {
    // Как у iPhone: после hvcC/dvvC/colr/amve идут 4 нулевых байта.
    for tail_len in 1..SHORT_TAIL_LIMIT {
        let stsd = parsed(read_hvc1_stsd_with_tail(&vec![0; tail_len]));
        let entry = stsd.visual_sample_entry().expect("hvc1 is a visual entry");
        assert_eq!(
            entry.codec_id, CODEC_ID_HEVC,
            "tail of {tail_len} zero bytes"
        );
        assert_eq!(
            entry.extra_data.len(),
            1,
            "hvcC must survive the terminator"
        );
    }
}

#[test]
fn nonzero_short_tail_after_sample_entry_children_is_still_truncation() {
    for tail in [[0, 0, 0, 1], [0, 0, 0, 0xff], [0xff, 0, 0, 0]] {
        assert!(
            matches!(
                read_hvc1_stsd_with_tail(&tail),
                Err(AtomError::UnexpectedEndOfAtom)
            ),
            "tail {tail:?} must stay a structural error"
        );
    }
}

#[test]
fn zero_short_tail_is_still_truncation_outside_sample_entries() {
    // Общий `next_header` правила терминатора не знает: `free` + 4 нуля на верхнем уровне
    // по-прежнему обрыв.
    let mut bytes = atom_bytes(*b"free", &[]);
    bytes.extend_from_slice(&[0; 4]);
    let mut iterator = iterator(bytes);
    assert!(matches!(iterator.next_header(), Ok(Some(_))));
    assert!(matches!(
        iterator.next_header(),
        Err(AtomError::UnexpectedEndOfAtom)
    ));
}
