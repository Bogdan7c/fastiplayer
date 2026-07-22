use std::io::Cursor;

use symphonia_core::io::MediaSourceStream;

use super::TfdtAtom;
use crate::atoms::{AtomError, AtomIterator};

/// Создаёт полный synthetic atom с указанным payload.
fn atom_bytes(atom_type: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let atom_size = u32::try_from(payload.len() + 8).expect("test atom fits u32");
    let mut bytes = Vec::with_capacity(payload.len() + 8);
    bytes.extend_from_slice(&atom_size.to_be_bytes());
    bytes.extend_from_slice(&atom_type);
    bytes.extend_from_slice(payload);
    bytes
}

/// Парсит synthetic `tfdt` через production atom iterator.
fn parse_tfdt(payload: &[u8]) -> Result<TfdtAtom, AtomError> {
    let bytes = atom_bytes(*b"tfdt", payload);
    let source_len = bytes.len() as u64;
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut iterator = AtomIterator::new(source, Some(source_len));
    iterator.next_header()?.expect("tfdt header");
    iterator.read_atom::<TfdtAtom>()
}

#[test]
fn parses_version_zero_base_decode_time() {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&17_u32.to_be_bytes());

    let tfdt = match parse_tfdt(&payload) {
        Ok(tfdt) => tfdt,
        Err(_) => panic!("valid tfdt v0 должен парситься"),
    };

    assert_eq!(tfdt.base_media_decode_time, 17);
}

#[test]
fn parses_version_one_base_decode_time() {
    let mut payload = vec![1, 0, 0, 0];
    payload.extend_from_slice(&u64::from(u32::MAX).wrapping_add(9).to_be_bytes());

    let tfdt = match parse_tfdt(&payload) {
        Ok(tfdt) => tfdt,
        Err(_) => panic!("valid tfdt v1 должен парситься"),
    };

    assert_eq!(tfdt.base_media_decode_time, u64::from(u32::MAX) + 9);
}

#[test]
fn rejects_unsupported_version() {
    let payload = [2, 0, 0, 0];

    assert!(matches!(parse_tfdt(&payload), Err(AtomError::Other(_))));
}

#[test]
fn rejects_truncated_decode_time() {
    let payload = [1, 0, 0, 0, 0, 0, 0, 1];

    assert!(matches!(
        parse_tfdt(&payload),
        Err(AtomError::UnexpectedEndOfAtom)
    ));
}
