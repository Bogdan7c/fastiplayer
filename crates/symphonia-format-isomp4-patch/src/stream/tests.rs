use std::io::Cursor;

use symphonia_core::io::MediaSourceStream;

use crate::atoms::{AtomIterator, TrunAtom, hdlr::HandlerType};

use super::{apply_composition_offset, fragment_seek_sample_rel};

/// Парсит synthetic run с per-sample flags через production atom reader.
fn flagged_run(sample_flags: &[u32]) -> TrunAtom {
    let payload_size = 8 + sample_flags.len() * 4;
    let atom_size = u32::try_from(payload_size + 8).expect("test trun fits u32");
    let mut bytes = Vec::with_capacity(atom_size as usize);
    bytes.extend_from_slice(&atom_size.to_be_bytes());
    bytes.extend_from_slice(b"trun");
    bytes.extend_from_slice(&[0, 0, 4, 0]);
    bytes.extend_from_slice(&(sample_flags.len() as u32).to_be_bytes());
    for sample_flags in sample_flags {
        bytes.extend_from_slice(&sample_flags.to_be_bytes());
    }

    let source_len = bytes.len() as u64;
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut iterator = AtomIterator::new(source, Some(source_len));
    assert!(matches!(iterator.next_header(), Ok(Some(_))));
    match iterator.read_atom::<TrunAtom>() {
        Ok(trun) => trun,
        Err(_) => panic!("valid synthetic trun должен парситься"),
    }
}

#[test]
fn composition_offsets_keep_pts_distinct_from_dts() {
    let dts = 100;

    assert!(matches!(apply_composition_offset(dts, 7), Ok(107)));
    assert!(matches!(apply_composition_offset(dts, -4), Ok(96)));
}

#[test]
fn impossible_negative_pts_is_structural_error() {
    assert!(apply_composition_offset(3, -4).is_err());
}

#[test]
fn fragment_seek_requires_explicit_rap_only_for_video_handler() {
    const PROVEN_SYNC: u32 = 0x0200_0000;
    const NON_SYNC_DEPENDENT: u32 = 0x0101_0000;

    let unknown_video = [flagged_run(&[0, 0])];
    assert_eq!(
        fragment_seek_sample_rel(&HandlerType::Video, &unknown_video, &[0], 0, 1, 0),
        None
    );

    let explicit_video = [flagged_run(&[PROVEN_SYNC, NON_SYNC_DEPENDENT])];
    assert_eq!(
        fragment_seek_sample_rel(&HandlerType::Video, &explicit_video, &[0], 0, 1, 0),
        Some(0)
    );

    assert_eq!(
        fragment_seek_sample_rel(&HandlerType::Sound, &unknown_video, &[0], 0, 1, 0),
        Some(1)
    );
}
