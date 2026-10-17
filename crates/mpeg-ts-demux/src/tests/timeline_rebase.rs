//! Склейка двух записей, где видео второй записи приходит раньше звука:
//! предварительный (по шагу кадра) сдвиг эпохи уточняет первый аудиокадр, и
//! seek anchor IDR-а второй записи переезжает вместе с ним.

use super::*;

/// ADTS из `adts_frame`: 1024 сэмпла @ 44.1 кГц = 2089 тиков (с отбрасыванием дроби).
const ADTS_FRAME_90KHZ: u64 = 2_089;
/// Видео 10 к/с.
const VIDEO_STEP_90KHZ: u64 = 9_000;
/// Видеокадров в каждой записи (0.4 с: скачок назад на стыке больше допуска).
const VIDEO_FRAMES_PER_RECORDING: u64 = 4;
/// Аудиокадров в каждой записи (≈ 0.418 с).
const AUDIO_FRAMES_PER_RECORDING: u64 = 18;

/// SPS + PPS + IDR: каждый AU — самостоятельный keyframe.
const IDR_ACCESS_UNIT: [u8; 17] = [
    0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00, 0x00, 0x01, 0x65,
    0x88,
];

/// Две записи с одинаковыми «сырыми» timestamps (обе начинаются с 0). Во
/// второй первые три видео PES идут раньше звука (как у ffmpeg), последний —
/// после. Видео PES без объявленной длины закрывается следующим стартом PES, а
/// AU — следующим AU, поэтому до звука успевает выйти ровно IDR второй записи.
fn two_recordings_video_first() -> Vec<u8> {
    let mut builder = TsFixtureBuilder::new().pat(&[(1, PMT_PID)], 0).pmt(
        PMT_PID,
        1,
        &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)],
        0,
    );
    let video_pts = |frame: u64| frame * VIDEO_STEP_90KHZ;
    for _recording in 0..2 {
        builder = builder
            .pes(VIDEO_PID, video_pts(0), None, &IDR_ACCESS_UNIT)
            .pes(VIDEO_PID, video_pts(1), None, &IDR_ACCESS_UNIT)
            .pes(VIDEO_PID, video_pts(2), None, &IDR_ACCESS_UNIT);
        for frame in 0..AUDIO_FRAMES_PER_RECORDING {
            builder = builder.pes(
                AUDIO_PID,
                frame * ADTS_FRAME_90KHZ,
                None,
                &adts_frame(&[0x11, 0x22]),
            );
        }
        for frame in 3..VIDEO_FRAMES_PER_RECORDING {
            builder = builder.pes(VIDEO_PID, video_pts(frame), None, &IDR_ACCESS_UNIT);
        }
    }
    builder.finish()
}

fn pts_by_kind(events: &[DemuxReadEvent], kind: TrackKind) -> Vec<i64> {
    events
        .iter()
        .filter_map(|event| match event {
            DemuxReadEvent::Packet(packet) if packet.kind == kind => {
                packet.track_pts.map(|pts| pts.units.get())
            }
            _ => None,
        })
        .collect()
}

/// Конец звука первой записи — туда встаёт звук второй.
const FIRST_AUDIO_END: i64 = (AUDIO_FRAMES_PER_RECORDING * ADTS_FRAME_90KHZ) as i64;

#[test]
fn audio_refines_video_provisional_epoch_and_seek_anchor_moves_with_it() {
    let bytes = two_recordings_video_first();

    let mut forward = MpegTsDemuxer::open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes.clone()))),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open forward TS");
    let events = drain(&mut forward).expect("forward playback");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, DemuxReadEvent::TracksChanged(_)))
    );
    let audio = pts_by_kind(&events, TrackKind::Audio);
    let expected_audio: Vec<i64> = (0..2 * AUDIO_FRAMES_PER_RECORDING)
        .map(|frame| (frame * ADTS_FRAME_90KHZ) as i64)
        .collect();
    assert_eq!(audio, expected_audio, "audio is seamless across the join");
    // Первый AU второй записи вышел ещё по предварительному сдвигу (по шагу
    // видео — сразу за последним кадром первой записи), следующие — уже по
    // уточнённому звуком: звук второй записи начинается ровно в конце первого.
    let video = pts_by_kind(&events, TrackKind::Video);
    let step = VIDEO_STEP_90KHZ as i64;
    let frames = VIDEO_FRAMES_PER_RECORDING as i64;
    let provisional_idr = frames * step;
    let mut expected_video: Vec<i64> = (0..frames).map(|frame| frame * step).collect();
    expected_video.push(provisional_idr);
    expected_video.extend((1..frames).map(|frame| FIRST_AUDIO_END + frame * step));
    assert_eq!(video, expected_video);
    assert_ne!(
        provisional_idr, FIRST_AUDIO_END,
        "fixture: refinement moves"
    );

    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("two-recordings.ts");
    std::fs::write(&path, &bytes).expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open source");
    let mut seekable = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open seekable TS");
    // Чуть позже начала второй записи на уточнённой шкале.
    let target = Duration::from_micros(((FIRST_AUDIO_END + 900) as u64) * 100 / 9);
    let result = seekable
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("seek to second recording");
    // Anchor IDR-а второй записи перенесён на уточнённый сдвиг; без переноса
    // seek вернул бы предварительные 18000 и дал A/V сдвиг на разницу.
    assert_eq!(
        result
            .actual_track_timestamp
            .map(|timestamp| timestamp.units.get()),
        Some(FIRST_AUDIO_END)
    );
    let after_seek = drain(&mut seekable).expect("playback after seek");
    assert_eq!(
        pts_by_kind(&after_seek, TrackKind::Video),
        (0..frames)
            .map(|frame| FIRST_AUDIO_END + frame * step)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        pts_by_kind(&after_seek, TrackKind::Audio),
        expected_audio[AUDIO_FRAMES_PER_RECORDING as usize..]
    );
}
