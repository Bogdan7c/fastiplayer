//! Seek: попадание на границу кадра не позже цели, точный PTS, откат при отказе.

use std::time::Duration;

use ac3_elementary_demux::Ac3ElementaryDemuxError;
use media_core::{DemuxSeekRequest, Demuxer, MediaTime};

use crate::support::{
    FIXTURE_FRAMES, STEREO_AC3, STEREO_FRAME_BYTES, SURROUND_EAC3, SURROUND_EAC3_FRAME_BYTES,
    drain_packets, frame_start, next_packet, open_direct, seekable_input, stream_input,
};

/// Seek в середину (0.25 с внутри кадра 7) → кадр 7 с PTS 0.224 с и его же байтами.
#[test]
fn seek_to_middle_lands_on_frame_boundary_with_exact_pts() {
    for (stream, frame_bytes) in [
        (STEREO_AC3, STEREO_FRAME_BYTES),
        (SURROUND_EAC3, SURROUND_EAC3_FRAME_BYTES),
    ] {
        let mut demuxer = open_direct(seekable_input(stream));
        let _ = next_packet(&mut demuxer);
        let target = Duration::from_millis(250);
        let result = demuxer.seek(target).expect("seek");
        assert_eq!(result.requested_position, MediaTime::from_duration(target));
        assert_eq!(
            result.actual_position,
            MediaTime::from_duration(frame_start(7))
        );
        assert_eq!(
            result
                .actual_track_timestamp
                .expect("track timestamp")
                .units
                .get(),
            7 * 1536
        );

        let packet = next_packet(&mut demuxer);
        assert_eq!(packet.pts, frame_start(7));
        assert_eq!(packet.byte_offset, Some(7 * frame_bytes as u64));
        assert_eq!(
            packet.data.as_ref(),
            &stream[7 * frame_bytes..8 * frame_bytes]
        );
        // После seek-а время продолжает идти от точки посадки.
        let rest = drain_packets(&mut demuxer);
        assert_eq!(rest.len(), FIXTURE_FRAMES - 8);
        assert_eq!(rest[0].pts, frame_start(8));
    }
}

/// Цель ровно на начале кадра — сам этот кадр; режимы seek-а ведут себя одинаково.
#[test]
fn seek_exactly_on_boundary_and_decode_point_mode_agree() {
    let mut demuxer = open_direct(seekable_input(STEREO_AC3));
    let accurate = demuxer.seek(frame_start(8)).expect("accurate seek");
    assert_eq!(
        accurate.actual_position,
        MediaTime::from_duration(frame_start(8))
    );
    assert_eq!(next_packet(&mut demuxer).pts, frame_start(8));

    let decode_point = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(frame_start(8)))
        .expect("decode point seek");
    assert_eq!(decode_point.actual_position, accurate.actual_position);
}

#[test]
fn seek_to_start_and_past_end() {
    let mut demuxer = open_direct(seekable_input(STEREO_AC3));
    drain_packets(&mut demuxer);

    let start = demuxer.seek(Duration::ZERO).expect("seek to start");
    assert_eq!(
        start.actual_position,
        MediaTime::from_duration(Duration::ZERO)
    );
    assert_eq!(drain_packets(&mut demuxer).len(), FIXTURE_FRAMES);

    // За концом: последний целый кадр, дальше EOF.
    let past_end = demuxer
        .seek(Duration::from_secs(60))
        .expect("seek past end");
    assert_eq!(
        past_end.actual_position,
        MediaTime::from_duration(frame_start(15))
    );
    let rest = drain_packets(&mut demuxer);
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].pts, frame_start(15));
}

/// Обрезанный хвост: seek за конец находит последний *целый* кадр.
#[test]
fn seek_past_end_of_truncated_file_finds_last_whole_frame() {
    let truncated = &STEREO_AC3[..15 * STEREO_FRAME_BYTES + 300];
    let mut demuxer = open_direct(seekable_input(truncated));
    let result = demuxer.seek(Duration::from_secs(60)).expect("seek");
    assert_eq!(
        result.actual_position,
        MediaTime::from_duration(frame_start(14))
    );
    let rest = drain_packets(&mut demuxer);
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].byte_offset, Some(14 * STEREO_FRAME_BYTES as u64));
}

/// Отказ seek-а на потоке без seek-а — typed и не трогает позицию чтения.
#[test]
fn not_seekable_rejection_keeps_read_position() {
    let mut demuxer = open_direct(stream_input(STEREO_AC3));
    for _ in 0..3 {
        let _ = next_packet(&mut demuxer);
    }
    let error = demuxer.seek(Duration::ZERO).expect_err("must not seek");
    assert!(matches!(
        error.downcast_ref::<Ac3ElementaryDemuxError>(),
        Some(Ac3ElementaryDemuxError::NotSeekable)
    ));
    let packet = next_packet(&mut demuxer);
    assert_eq!(packet.pts, frame_start(3));
    assert_eq!(packet.byte_offset, Some(3 * STEREO_FRAME_BYTES as u64));
}

/// Seek с ID3v2 впереди: позиции считаются от начала данных, а не файла.
#[test]
fn seek_after_leading_id3_tag_uses_data_start() {
    let mut bytes = b"ID3\x03\x00\x00\x00\x00\x01\x00".to_vec();
    bytes.extend(std::iter::repeat_n(0x0B_u8, 128));
    bytes.extend_from_slice(STEREO_AC3);
    let tag_bytes = 10 + 128;
    let mut demuxer = open_direct(seekable_input(&bytes));
    assert_eq!(demuxer.duration(), Some(frame_start(FIXTURE_FRAMES as u64)));
    let result = demuxer.seek(Duration::from_millis(100)).expect("seek");
    assert_eq!(
        result.actual_position,
        MediaTime::from_duration(frame_start(3))
    );
    let packet = next_packet(&mut demuxer);
    assert_eq!(
        packet.byte_offset,
        Some((tag_bytes + 3 * STEREO_FRAME_BYTES) as u64)
    );
    assert_eq!(packet.pts, frame_start(3));
}
