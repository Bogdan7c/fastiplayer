//! Тесты признаков шифрования на настоящих init-сегментах и их «зашифрованных» копиях.

use super::{InconsistentInitialization, ProtectionVerdict, scan_initialization};
use crate::container::PushedContainer;
use crate::framing::ebml::{UNKNOWN_SIZE_FIELD, encode_id, encode_known_size_field};

const VIDEO_MP4: &[u8] = include_bytes!("../../browser-bridge-protocol/fixtures/media/video.mp4");
const AUDIO_MP4: &[u8] = include_bytes!("../../browser-bridge-protocol/fixtures/media/audio.mp4");
const MUXED_WEBM: &[u8] = include_bytes!("../../browser-bridge-protocol/fixtures/media/muxed.webm");
/// Конец init-сегмента (`ftyp`+`moov`) в `VIDEO_MP4`; у `AUDIO_MP4` он свой.
const VIDEO_INIT_END: usize = 733;

/// Init-сегмент fMP4: `ftyp` + `moov` в начале файла.
fn mp4_initialization(file: &[u8]) -> Vec<u8> {
    let ftyp_len = u32::from_be_bytes([file[0], file[1], file[2], file[3]]) as usize;
    let moov = &file[ftyp_len..];
    let moov_len = u32::from_be_bytes([moov[0], moov[1], moov[2], moov[3]]) as usize;
    file[..ftyp_len + moov_len].to_vec()
}

/// Переименовывает первое описание дорожки внутри `moov` (как это делает CENC).
fn rename_sample_entry(mut initialization: Vec<u8>, from: &[u8; 4], to: &[u8; 4]) -> Vec<u8> {
    let ftyp_len = u32::from_be_bytes([
        initialization[0],
        initialization[1],
        initialization[2],
        initialization[3],
    ]) as usize;
    let position = initialization[ftyp_len..]
        .windows(4)
        .position(|window| window == from)
        .expect("sample entry inside moov")
        + ftyp_len;
    initialization[position..position + 4].copy_from_slice(to);
    initialization
}

/// Элемент EBML с 8-байтовым полем размера.
fn element(id: u32, body: &[u8]) -> Vec<u8> {
    let mut bytes = encode_id(id);
    bytes.extend_from_slice(&encode_known_size_field(body.len() as u64).expect("small"));
    bytes.extend_from_slice(body);
    bytes
}

/// Init WebM: настоящий EBML-заголовок фикстуры + `Segment` неизвестного размера + `Tracks`.
fn webm_initialization(track_entry_children: &[u8]) -> Vec<u8> {
    let ebml_header_len = 36; // Проверено разбором фикстуры в тестах сканера.
    let mut initialization = MUXED_WEBM[..ebml_header_len].to_vec();
    initialization.extend(encode_id(0x1853_8067));
    initialization.extend_from_slice(&UNKNOWN_SIZE_FIELD);
    let track_number = element(0xD7, &[1]);
    let mut entry_body = track_number;
    entry_body.extend_from_slice(track_entry_children);
    initialization.extend(element(0x1654_AE6B, &element(0xAE, &entry_body)));
    initialization
}

#[test]
fn clear_fixtures_are_clear() {
    for file in [VIDEO_MP4, AUDIO_MP4] {
        assert_eq!(
            scan_initialization(PushedContainer::FragmentedMp4, &mp4_initialization(file)),
            Ok(ProtectionVerdict::Clear)
        );
    }
    assert_eq!(mp4_initialization(VIDEO_MP4).len(), VIDEO_INIT_END);
    assert_eq!(
        scan_initialization(PushedContainer::WebM, &webm_initialization(&[])),
        Ok(ProtectionVerdict::Clear)
    );
    assert_eq!(
        scan_initialization(PushedContainer::MpegTs, &[]),
        Ok(ProtectionVerdict::Clear)
    );
}

#[test]
fn cenc_renamed_video_and_audio_entries_are_protected() {
    let video = rename_sample_entry(mp4_initialization(VIDEO_MP4), b"avc1", b"encv");
    assert_eq!(
        scan_initialization(PushedContainer::FragmentedMp4, &video),
        Ok(ProtectionVerdict::Protected)
    );
    let audio = rename_sample_entry(mp4_initialization(AUDIO_MP4), b"mp4a", b"enca");
    assert_eq!(
        scan_initialization(PushedContainer::FragmentedMp4, &audio),
        Ok(ProtectionVerdict::Protected)
    );
}

#[test]
fn webm_content_encryption_is_protected() {
    let encryption = element(0x5035, &element(0x47E1, &[5])); // ContentEncAlgo = AES.
    let encodings = element(0x6D80, &element(0x6240, &encryption));
    assert_eq!(
        scan_initialization(PushedContainer::WebM, &webm_initialization(&encodings)),
        Ok(ProtectionVerdict::Protected)
    );
}

#[test]
fn inconsistent_structure_is_an_error_not_clear() {
    let mut truncated = mp4_initialization(VIDEO_MP4);
    truncated.truncate(VIDEO_INIT_END - 50);
    assert_eq!(
        scan_initialization(PushedContainer::FragmentedMp4, &truncated),
        Err(InconsistentInitialization)
    );
    let mut webm = webm_initialization(&[]);
    webm.truncate(webm.len() - 2);
    assert_eq!(
        scan_initialization(PushedContainer::WebM, &webm),
        Err(InconsistentInitialization)
    );
}
