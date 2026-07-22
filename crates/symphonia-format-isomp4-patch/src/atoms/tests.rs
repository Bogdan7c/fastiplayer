use std::io::Cursor;

use symphonia_core::io::MediaSourceStream;

use super::{AtomError, AtomIterator};

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
