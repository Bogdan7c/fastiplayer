//! Fastiplayer: размер `avcC`/`hvcC` больше 1 КиБ (запись x265 с SEI) читается целиком,
//! а заголовок выше потолка `MAX_DECODER_CONFIGURATION_RECORD_SIZE` по-прежнему отвергается.
//! Проверка идёт через настоящий маршрут `stsd` → sample entry → дочерний атом.

use std::io::Cursor;

use symphonia_core::codecs::video::well_known::{CODEC_ID_H264, CODEC_ID_HEVC};
use symphonia_core::io::MediaSourceStream;

use super::limits::MAX_DECODER_CONFIGURATION_RECORD_SIZE;
use super::{AtomError, AtomIterator, StsdAtom};
use symphonia_core::errors::Error;

/// Размер SEI-строки настроек, как у реального x265 (`hvcC` ≈ 2,5 КиБ).
const X265_LIKE_SEI_BYTES: usize = 2_400;

fn atom_bytes(atom_type: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len() + 8).expect("test atom size fits u32");
    let mut bytes = size.to_be_bytes().to_vec();
    bytes.extend_from_slice(&atom_type);
    bytes.extend_from_slice(payload);
    bytes
}

/// VisualSampleEntry (ISO/IEC 14496-12 §12.1.3) 64×64 с заданными дочерними атомами.
fn visual_sample_entry(child_atoms: &[u8]) -> Vec<u8> {
    let mut payload = vec![0; 6];
    payload.extend_from_slice(&1_u16.to_be_bytes());
    payload.extend_from_slice(&[0; 16]);
    payload.extend_from_slice(&64_u16.to_be_bytes());
    payload.extend_from_slice(&64_u16.to_be_bytes());
    payload.extend_from_slice(&0x0048_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0x0048_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&1_u16.to_be_bytes());
    payload.extend_from_slice(&[0; 32]);
    payload.extend_from_slice(&24_u16.to_be_bytes());
    payload.extend_from_slice(&0xffff_u16.to_be_bytes());
    payload.extend_from_slice(child_atoms);
    payload
}

/// HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 §8.3.3.1): Main, level 4.1 и один
/// массив prefix-SEI (NAL type 39) с длинным payload — так x265 пишет строку настроек.
fn hevc_record_with_sei(sei_bytes: usize) -> Vec<u8> {
    let mut record = vec![
        0x01, // configurationVersion
        0x01, // profile_space 0, tier 0, general_profile_idc 1 (Main)
        0x60, 0x00, 0x00, 0x00, // general_profile_compatibility_flags
        0x90, 0x00, 0x00, 0x00, 0x00, 0x00, // general_constraint_indicator_flags
        0x7b, // general_level_idc = 123 (4.1)
        0xf0, 0x00, // min_spatial_segmentation_idc
        0xfc, // parallelismType
        0xfd, // chroma_format_idc = 1
        0xf8, 0xf8, // bit depth luma/chroma = 8
        0x00, 0x00, // avgFrameRate
        0x0f, // lengthSizeMinusOne = 3
        0x01, // numOfArrays
        0x27, // array_completeness 0, NAL_unit_type 39 (prefix SEI)
        0x00, 0x01, // numNalus
    ];
    let length = u16::try_from(sei_bytes).expect("SEI length fits u16");
    record.extend_from_slice(&length.to_be_bytes());
    record.extend(std::iter::repeat_n(0x55, sei_bytes));
    record
}

fn read_stsd(
    sample_entry_type: [u8; 4],
    configuration_atom: [u8; 4],
    record: &[u8],
) -> super::Result<StsdAtom> {
    let entry = atom_bytes(
        sample_entry_type,
        &visual_sample_entry(&atom_bytes(configuration_atom, record)),
    );
    let mut stsd_payload = vec![0; 4];
    stsd_payload.extend_from_slice(&1_u32.to_be_bytes());
    stsd_payload.extend_from_slice(&entry);
    let bytes = atom_bytes(*b"stsd", &stsd_payload);
    let length = bytes.len() as u64;
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut iterator = AtomIterator::new(source, Some(length));
    if iterator.next_header()?.is_none() {
        panic!("synthetic stsd has a header");
    }
    iterator.read_atom::<StsdAtom>()
}

/// Разворачивает результат без `Debug` у `AtomError`: печатаем только symphonia-ошибку.
fn parsed(result: super::Result<StsdAtom>) -> StsdAtom {
    match result {
        Ok(stsd) => stsd,
        Err(AtomError::Other(error)) => panic!("stsd must parse: {error}"),
        Err(_) => panic!("stsd must parse: structural atom error"),
    }
}

#[test]
fn x265_sized_hvcc_is_read_whole_and_handed_to_the_decoder_byte_for_byte() {
    let record = hevc_record_with_sei(X265_LIKE_SEI_BYTES);
    assert!(
        record.len() > 1024,
        "fixture must exceed the old upstream limit"
    );

    let stsd = parsed(read_stsd(*b"hvc1", *b"hvcC", &record));
    let entry = stsd
        .visual_sample_entry()
        .expect("hvc1 is a visual sample entry");

    assert_eq!(entry.codec_id, CODEC_ID_HEVC);
    assert_eq!(entry.level, Some(123));
    assert_eq!(entry.extra_data.len(), 1);
    assert_eq!(entry.extra_data[0].data.as_ref(), record.as_slice());
}

#[test]
fn avcc_larger_than_one_kib_is_accepted_like_hvcc() {
    // AVCDecoderConfigurationRecord: High@4.1, один SPS на 1,5 КиБ (длины SPS 16-битные).
    let mut record = vec![0x01, 0x64, 0x00, 0x29, 0xff, 0xe1];
    record.extend_from_slice(&1_500_u16.to_be_bytes());
    record.extend_from_slice(&[0x67, 0x64, 0x00, 0x29]);
    record.extend(std::iter::repeat_n(0xaa, 1_496));
    record.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xeb, 0xe3, 0xcb]);
    assert!(record.len() > 1024);

    let stsd = parsed(read_stsd(*b"avc1", *b"avcC", &record));
    let entry = stsd
        .visual_sample_entry()
        .expect("avc1 is a visual sample entry");

    assert_eq!(entry.codec_id, CODEC_ID_H264);
    assert_eq!(entry.extra_data[0].data.as_ref(), record.as_slice());
}

#[test]
fn configuration_record_above_the_ceiling_is_still_rejected() {
    let too_large = usize::try_from(MAX_DECODER_CONFIGURATION_RECORD_SIZE).expect("fits usize") + 1;
    let mut record = hevc_record_with_sei(16);
    record.resize(too_large, 0);

    match read_stsd(*b"hvc1", *b"hvcC", &record) {
        Err(AtomError::Other(Error::DecodeError(message))) => {
            assert!(message.contains("greater than 64 KiB"), "{message}");
        }
        Err(AtomError::Other(error)) => panic!("unexpected error: {error}"),
        Err(_) => panic!("unexpected structural error"),
        Ok(_) => panic!("oversized hvcC must be rejected"),
    }
}
