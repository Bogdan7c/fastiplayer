//! Тесты нормализации packed audio: сама TS-обёртка и разбор ID3 `PRIV`.
//!
//! Приём «ресурс -> demux -> пакеты с PTS» проверяют интеграционные тесты
//! `tests/packed_audio_hls.rs`; здесь - контракт самого модуля на байтах.

use std::time::Duration;

use super::{
    PackedAudioError, SegmentTimestampFallback, looks_like_packed_audio,
    normalize_to_transport_stream,
};

const OWNER: &str = "com.apple.streaming.transportStreamTimestamp";

/// ADTS-кадр AAC-LC 44.1 кГц стерео с заданным числом байт raw-данных.
fn adts_frame(raw_len: usize) -> Vec<u8> {
    let frame_length = 7 + raw_len;
    let mut frame = vec![
        0xff,
        0xf1,
        0x50,
        0x80 | ((frame_length >> 11) as u8 & 0x03),
        (frame_length >> 3) as u8,
        ((frame_length & 0x07) as u8) << 5 | 0x1f,
        0xfc,
    ];
    frame.resize(frame_length, 0x11);
    frame
}

/// Заголовок кадра MPEG-1 Layer III 128 кбит/с 44.1 кГц.
const MP3_HEADER: [u8; 4] = [0xff, 0xfb, 0x90, 0x00];

fn synchsafe_bytes(value: usize) -> [u8; 4] {
    [
        ((value >> 21) & 0x7f) as u8,
        ((value >> 14) & 0x7f) as u8,
        ((value >> 7) & 0x7f) as u8,
        (value & 0x7f) as u8,
    ]
}

/// ID3v2.4 тег с одним PRIV-кадром `transportStreamTimestamp`.
fn id3_v24_with_timestamp(timestamp: u64) -> Vec<u8> {
    let mut payload = OWNER.as_bytes().to_vec();
    payload.push(0);
    payload.extend_from_slice(&timestamp.to_be_bytes());
    let mut frame = b"PRIV".to_vec();
    frame.extend_from_slice(&synchsafe_bytes(payload.len()));
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&payload);
    let mut tag = b"ID3".to_vec();
    tag.extend_from_slice(&[4, 0, 0]);
    tag.extend_from_slice(&synchsafe_bytes(frame.len()));
    tag.extend_from_slice(&frame);
    tag
}

/// То же в ID3v2.3: размер кадра обычный big-endian.
fn id3_v23_with_timestamp(timestamp: u64) -> Vec<u8> {
    let mut payload = OWNER.as_bytes().to_vec();
    payload.push(0);
    payload.extend_from_slice(&timestamp.to_be_bytes());
    let mut frame = b"PRIV".to_vec();
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&payload);
    let mut tag = b"ID3".to_vec();
    tag.extend_from_slice(&[3, 0, 0]);
    tag.extend_from_slice(&synchsafe_bytes(frame.len()));
    tag.extend_from_slice(&frame);
    tag
}

/// Склеивает payload-ы всех TS-пакетов заданного PID (без заголовков и stuffing-а).
fn pid_payload(ts: &[u8], wanted_pid: u16) -> Vec<u8> {
    assert_eq!(ts.len() % 188, 0, "TS должен состоять из целых пакетов");
    let mut payload = Vec::new();
    for packet in ts.chunks(188) {
        assert_eq!(packet[0], 0x47, "sync byte");
        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        if pid != wanted_pid {
            continue;
        }
        let payload_start = if packet[3] & 0x20 != 0 {
            5 + usize::from(packet[4])
        } else {
            4
        };
        payload.extend_from_slice(&packet[payload_start..]);
    }
    payload
}

/// `stream_type` единственного потока PMT: pointer_field(1) + заголовок секции до цикла (12).
fn pmt_stream_type(ts: &[u8]) -> u8 {
    pid_payload(ts, 0x1000)[13]
}

/// Достаёт из TS-вывода PTS первого аудио-PES и склеенный elementary payload.
fn audio_pes_pts_and_payload(ts: &[u8]) -> (u64, Vec<u8>) {
    let pes = pid_payload(ts, 0x0100);
    assert_eq!(&pes[..4], &[0, 0, 1, 0xc0], "аудио-PES со stream_id 0xC0");
    let pts = (u64::from((pes[9] >> 1) & 0x07) << 30)
        | (u64::from(pes[10]) << 22)
        | (u64::from(pes[11] >> 1) << 15)
        | (u64::from(pes[12]) << 7)
        | u64::from(pes[13] >> 1);
    (pts, pes[14..].to_vec())
}

#[test]
fn id3_priv_timestamp_becomes_pes_pts_and_audio_survives_unchanged() {
    let audio = [adts_frame(20), adts_frame(30)].concat();
    let segment = [id3_v24_with_timestamp(900_000), audio.clone()].concat();

    let ts = normalize_to_transport_stream(&segment, SegmentTimestampFallback::Zero)
        .expect("packed AAC с ID3 нормализуется");

    let (pts, payload) = audio_pes_pts_and_payload(&ts);
    assert_eq!(pts, 900_000, "PTS берётся из ID3 PRIV, а не из fallback");
    assert_eq!(payload, audio, "ADTS-кадры доходят без изменений и без ID3");
}

#[test]
fn id3v23_priv_and_33_bit_wrap_are_supported() {
    // Значение 2^33 + 5 в PRIV обрезается до 33 бит, как требует PES PTS.
    let timestamp = (1_u64 << 33) + 5;
    let segment = [id3_v23_with_timestamp(timestamp), adts_frame(10)].concat();

    let ts = normalize_to_transport_stream(&segment, SegmentTimestampFallback::Zero)
        .expect("ID3v2.3 тоже разбирается");

    assert_eq!(audio_pes_pts_and_payload(&ts).0, 5);
}

#[test]
fn missing_priv_uses_manifest_timeline_start_in_ticks() {
    let segment = adts_frame(10);

    let ts = normalize_to_transport_stream(
        &segment,
        SegmentTimestampFallback::ManifestTimelineStart(Duration::from_millis(12_500)),
    )
    .expect("сегмент без ID3 тоже нормализуется");

    assert_eq!(audio_pes_pts_and_payload(&ts).0, 12_500 * 90);
}

#[test]
fn foreign_priv_owner_is_ignored_and_fallback_is_used() {
    let mut payload = b"com.example.other".to_vec();
    payload.push(0);
    payload.extend_from_slice(&777_u64.to_be_bytes());
    let mut frame = b"PRIV".to_vec();
    frame.extend_from_slice(&synchsafe_bytes(payload.len()));
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&payload);
    let mut tag = b"ID3".to_vec();
    tag.extend_from_slice(&[4, 0, 0]);
    tag.extend_from_slice(&synchsafe_bytes(frame.len()));
    tag.extend_from_slice(&frame);
    let segment = [tag, adts_frame(10)].concat();

    let ts = normalize_to_transport_stream(
        &segment,
        SegmentTimestampFallback::ManifestTimelineStart(Duration::from_secs(2)),
    )
    .expect("чужой PRIV не ломает сегмент");

    assert_eq!(audio_pes_pts_and_payload(&ts).0, 2 * 90_000);
}

#[test]
fn consecutive_id3_tags_are_all_skipped() {
    let segment = [
        id3_v24_with_timestamp(4_500),
        id3_v24_with_timestamp(9_999),
        adts_frame(10),
    ]
    .concat();

    let ts = normalize_to_transport_stream(&segment, SegmentTimestampFallback::Zero)
        .expect("несколько тегов подряд");

    let (pts, payload) = audio_pes_pts_and_payload(&ts);
    assert_eq!(pts, 4_500, "берётся первая метка");
    assert_eq!(payload, adts_frame(10));
}

#[test]
fn mp3_segment_gets_mpeg_audio_stream_type() {
    let mut mp3 = MP3_HEADER.to_vec();
    mp3.resize(417, 0);
    let segment = [id3_v24_with_timestamp(90_000), mp3.clone()].concat();

    let ts = normalize_to_transport_stream(&segment, SegmentTimestampFallback::Zero)
        .expect("packed MP3");

    assert_eq!(pmt_stream_type(&ts), 0x03, "MPEG-1 audio stream_type");
    assert_eq!(audio_pes_pts_and_payload(&ts).1, mp3);
}

#[test]
fn aac_segment_gets_adts_stream_type() {
    let ts = normalize_to_transport_stream(&adts_frame(10), SegmentTimestampFallback::Zero)
        .expect("packed AAC");
    assert_eq!(pmt_stream_type(&ts), 0x0f, "AAC ADTS stream_type");
}

#[test]
fn ac3_is_rejected_with_typed_error() {
    let segment = [id3_v24_with_timestamp(0), vec![0x0b, 0x77, 0, 0, 0, 0]].concat();
    assert_eq!(
        normalize_to_transport_stream(&segment, SegmentTimestampFallback::Zero),
        Err(PackedAudioError::UnsupportedCodec)
    );
}

#[test]
fn id3_without_audio_sync_and_truncated_id3_are_typed_errors() {
    let junk = [id3_v24_with_timestamp(0), vec![1, 2, 3, 4]].concat();
    assert_eq!(
        normalize_to_transport_stream(&junk, SegmentTimestampFallback::Zero),
        Err(PackedAudioError::MissingAudioSync)
    );
    let mut truncated = id3_v24_with_timestamp(0);
    truncated.truncate(truncated.len() - 3);
    assert_eq!(
        normalize_to_transport_stream(&truncated, SegmentTimestampFallback::Zero),
        Err(PackedAudioError::TruncatedId3)
    );
}

#[test]
fn sniffing_separates_packed_audio_from_ts_and_fmp4() {
    assert!(looks_like_packed_audio(b"ID3\x04\x00"));
    assert!(looks_like_packed_audio(&[0xff, 0xf1, 0x50]));
    assert!(looks_like_packed_audio(&[0xff, 0xfb, 0x90]));
    assert!(!looks_like_packed_audio(&[0x47, 0x40, 0x00]), "MPEG-TS");
    assert!(
        !looks_like_packed_audio(&[0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p']),
        "fMP4"
    );
    assert!(!looks_like_packed_audio(&[0xff, 0x01]), "короткий префикс");
}

#[test]
fn large_segment_keeps_every_byte_across_ts_packets() {
    let audio: Vec<u8> = (0..70_000_u32).map(|value| (value % 251) as u8).collect();
    let mut segment = vec![0xff, 0xf1, 0x50, 0x80, 0x00, 0x1f, 0xfc];
    segment.extend_from_slice(&audio);

    let ts = normalize_to_transport_stream(&segment, SegmentTimestampFallback::Zero)
        .expect("большой сегмент");

    assert_eq!(audio_pes_pts_and_payload(&ts).1, segment);
}
