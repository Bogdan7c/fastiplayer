use std::io::Cursor;

use symphonia_core::errors::Error;
use symphonia_core::io::MediaSourceStream;

use super::TrafAtom;
use crate::atoms::{AtomError, AtomIterator};

/// Оборачивает payload в ISO box.
fn atom(atom_type: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len() + 8).expect("test atom fits u32");
    let mut bytes = Vec::with_capacity(payload.len() + 8);
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(&atom_type);
    bytes.extend_from_slice(payload);
    bytes
}

/// Минимальный track fragment header для track 1.
fn tfhd() -> Vec<u8> {
    atom(*b"tfhd", &[0, 1, 0, 0, 0, 0, 0, 1])
}

/// Минимальный valid v0 decode-time box.
fn tfdt(base_decode_time: u32) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&base_decode_time.to_be_bytes());
    atom(*b"tfdt", &payload)
}

/// Парсит synthetic `traf` production parser-ом.
fn parse_traf(children: &[u8]) -> Result<TrafAtom, AtomError> {
    let bytes = atom(*b"traf", children);
    let source_len = bytes.len() as u64;
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut iterator = AtomIterator::new(source, Some(source_len));
    iterator.next_header()?.expect("traf header");
    iterator.read_atom::<TrafAtom>()
}

/// Проверяет stable decode-error reason без требования `Debug` от upstream error wrapper-а.
fn assert_decode_error(result: Result<TrafAtom, AtomError>, expected: &'static str) {
    match result {
        Err(AtomError::Other(Error::DecodeError(actual))) => assert_eq!(actual, expected),
        _ => panic!("ожидался exact traf decode error"),
    }
}

#[test]
fn requires_tfdt_for_profile_fragment() {
    assert_decode_error(parse_traf(&tfhd()), "isomp4 (traf): missing tfdt atom");
}

#[test]
fn rejects_duplicate_tfdt() {
    let mut children = tfhd();
    children.extend_from_slice(&tfdt(0));
    children.extend_from_slice(&tfdt(10));

    assert_decode_error(parse_traf(&children), "isomp4 (traf): duplicate tfdt atom");
}

#[test]
fn keeps_nonzero_base_decode_time() {
    let mut children = tfhd();
    children.extend_from_slice(&tfdt(4_096));

    let traf = match parse_traf(&children) {
        Ok(traf) => traf,
        Err(_) => panic!("valid traf должен парситься"),
    };
    assert_eq!(traf.tfdt.base_media_decode_time, 4_096);
}

#[test]
fn non_empty_fragment_requires_trun() {
    let non_empty_tfhd = atom(*b"tfhd", &[0, 0, 0, 0, 0, 0, 0, 1]);
    let mut children = non_empty_tfhd;
    children.extend_from_slice(&tfdt(0));

    assert_decode_error(parse_traf(&children), "isomp4 (traf): missing trun atom");
}
