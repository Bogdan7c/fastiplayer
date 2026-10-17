//! AC-3 посреди потока: восстановление после потери синхронизации (битый
//! заголовок, мусор между кадрами) с ограниченным поиском и смена формата
//! без lifecycle-сброса.

use super::ac3_elementary::{
    AC3_48K_FRAME_TICKS, ac3_stereo_frame, audio_packets, eac3_frame, open_bytes,
    program_with_descriptors, pts_ticks,
};
use super::*;

/// PTS i-го 48-кГц AC-3 кадра от нуля.
fn frame_pts(index: u64) -> u64 {
    index * AC3_48K_FRAME_TICKS
}

/// Синхрослово с перевёрнутым битом: так выглядит ошибка приёма в заголовке.
fn with_flipped_sync_bit(mut frame: Vec<u8>) -> Vec<u8> {
    frame[1] ^= 0x01;
    frame
}

/// Кадр с битым синхрословом посреди потока не роняет воспроизведение: он
/// пропускается, следующий проверенный syncframe снова захватывает поток,
/// время следующих кадров берётся из их PES (а не из цепочки, «съевшей» битый кадр).
#[test]
fn bit_flipped_syncframe_mid_stream_is_skipped_and_playback_continues() {
    let frames: Vec<Vec<u8>> = (1..=5).map(ac3_stereo_frame).collect();
    let bytes = program_with_descriptors(&[(0x81, AUDIO_PID, &[])])
        .pes(AUDIO_PID, frame_pts(0), None, &frames[0])
        .pes(AUDIO_PID, frame_pts(1), None, &frames[1])
        .pes(
            AUDIO_PID,
            frame_pts(2),
            None,
            &with_flipped_sync_bit(frames[2].clone()),
        )
        .pes(AUDIO_PID, frame_pts(3), None, &frames[3])
        .pes(AUDIO_PID, frame_pts(4), None, &frames[4])
        .finish();
    let mut demuxer = open_bytes(bytes);
    let events = drain(&mut *demuxer).expect("damaged frame must not stop playback");
    let packets = audio_packets(&events);
    let published: Vec<(&[u8], i64)> = packets
        .iter()
        .map(|packet| (packet.data.as_ref(), pts_ticks(packet)))
        .collect();
    let expected: Vec<(&[u8], i64)> = [0_usize, 1, 3, 4]
        .into_iter()
        .map(|index| (frames[index].as_slice(), frame_pts(index as u64) as i64))
        .collect();
    assert_eq!(published, expected);
}

/// Мусор посреди одного PES: кадры перед мусором публикуются по цепочке,
/// кадры после него в том же PES — нет (их время неизвестно: длительность
/// пропущенного звука не восстановить), а следующий PES снова даёт время.
#[test]
fn garbage_inside_pes_drops_frames_until_next_pes_timestamp() {
    let frames: Vec<Vec<u8>> = (1..=5).map(ac3_stereo_frame).collect();
    let damaged_pes = [
        frames[0].clone(),
        frames[1].clone(),
        vec![0x00; 16],
        frames[2].clone(),
        frames[3].clone(),
    ]
    .concat();
    let next_pes_pts = frame_pts(10);
    let bytes = program_with_descriptors(&[(0x81, AUDIO_PID, &[])])
        .pes(AUDIO_PID, 0, None, &damaged_pes)
        .pes(AUDIO_PID, next_pes_pts, None, &frames[4])
        .finish();
    let mut demuxer = open_bytes(bytes);
    let events = drain(&mut *demuxer).expect("garbage within the resync budget is recoverable");
    let packets = audio_packets(&events);
    let published: Vec<(&[u8], i64)> = packets
        .iter()
        .map(|packet| (packet.data.as_ref(), pts_ticks(packet)))
        .collect();
    assert_eq!(
        published,
        vec![
            (frames[0].as_slice(), 0),
            (frames[1].as_slice(), frame_pts(1) as i64),
            (frames[4].as_slice(), next_pes_pts as i64),
        ]
    );
}

/// Мусор длиннее предела поиска — уже не повреждение кадра, а не-AC-3 поток:
/// typed `Ac3SyncLost`, а не тихое бесконечное сканирование.
#[test]
fn garbage_beyond_resync_budget_is_typed_sync_loss() {
    let resync_limit = 256;
    let payload = [
        ac3_stereo_frame(1),
        ac3_stereo_frame(2),
        vec![0x00; resync_limit * 4],
        ac3_stereo_frame(3),
    ]
    .concat();
    let bytes = program_with_descriptors(&[(0x81, AUDIO_PID, &[])])
        .pes(AUDIO_PID, 0, None, &payload)
        .finish();
    let options = MpegTsDemuxOptions {
        ac3_resync_bytes: MpegTsLimit::new(resync_limit, "ac3_resync_bytes").expect("limit"),
        ..MpegTsDemuxOptions::default()
    };
    let error = MpegTsDemuxer::open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        CancellationToken::never_cancelled(),
        options,
    )
    .err()
    .expect("endless garbage must fail");
    let MpegTsDemuxError::Ac3SyncLost {
        pid,
        discarded_bytes,
        limit_bytes,
    } = error
    else {
        panic!("typed AC-3 sync loss expected, got {error:?}");
    };
    assert_eq!((pid, limit_bytes), (AUDIO_PID, resync_limit));
    assert!(discarded_bytes > resync_limit);
}

/// Тот же мусор в пределах (большего) предела по умолчанию восстанавливается:
/// предел — настройка, а не жёсткая константа.
#[test]
fn same_garbage_within_default_budget_recovers() {
    let payload = [ac3_stereo_frame(1), ac3_stereo_frame(2), vec![0x00; 1_024]].concat();
    let bytes = program_with_descriptors(&[(0x81, AUDIO_PID, &[])])
        .pes(AUDIO_PID, 0, None, &payload)
        .pes(AUDIO_PID, frame_pts(8), None, &ac3_stereo_frame(3))
        .finish();
    let mut demuxer = open_bytes(bytes);
    let events = drain(&mut *demuxer).expect("recoverable");
    let packets = audio_packets(&events);
    assert_eq!(packets.len(), 3);
    assert_eq!(pts_ticks(packets[2]), frame_pts(8) as i64);
}

/// Смена AC-3 stereo → E-AC-3 5.1 на одном PID-е: decoder семейства выбирает
/// синтаксис по `bsid` каждого packet-а, поэтому дорожка (и player) не
/// сбрасываются — `TracksChanged` не публикуется, все кадры доходят до decoder-а.
#[test]
fn codec_and_layout_switch_inside_ac3_family_keeps_track_without_lifecycle_reset() {
    let stereo = ac3_stereo_frame(1);
    let surround = [eac3_frame(0, 0, None, 2), eac3_frame(0, 0, None, 3)];
    let bytes = program_with_descriptors(&[(0x81, AUDIO_PID, &[])])
        .pes(AUDIO_PID, 0, None, &stereo)
        .pes(AUDIO_PID, frame_pts(1), None, &surround.concat())
        .finish();
    let mut demuxer = open_bytes(bytes);
    let opened_track = demuxer.tracks()[0].clone();
    assert_eq!(opened_track.codec_id, "A_AC3");
    assert_eq!(opened_track.channels, Some(2));

    let events = drain(&mut *demuxer).expect("drain");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, DemuxReadEvent::TracksChanged(_))),
        "format switch must not reset the player: {events:?}"
    );
    assert_eq!(demuxer.tracks(), [opened_track].as_slice());
    let packets = audio_packets(&events);
    let published: Vec<(&[u8], i64)> = packets
        .iter()
        .map(|packet| (packet.data.as_ref(), pts_ticks(packet)))
        .collect();
    assert_eq!(
        published,
        vec![
            (stereo.as_slice(), 0),
            (surround[0].as_slice(), frame_pts(1) as i64),
            (surround[1].as_slice(), frame_pts(2) as i64),
        ]
    );
}
