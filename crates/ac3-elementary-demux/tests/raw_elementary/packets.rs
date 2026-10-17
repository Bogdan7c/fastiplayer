//! Метаданные дорожки, нарезка, PTS и длительность.

use std::time::Duration;

use media_core::{DemuxSeekability, Demuxer, PacketKeyframe, TrackKind};

use crate::support::{
    FIXTURE_FRAMES, STEREO_AC3, STEREO_FRAME_BYTES, SURROUND_AC3_44K1, SURROUND_EAC3,
    SURROUND_EAC3_FRAME_BYTES, drain_packets, frame_start, open_direct, seekable_input,
    stream_input,
};

#[test]
fn stereo_ac3_track_metadata_and_exact_constant_bitrate_duration() {
    let demuxer = open_direct(seekable_input(STEREO_AC3));
    let [track] = demuxer.tracks() else {
        panic!("expected exactly one track");
    };
    assert_eq!(track.kind, TrackKind::Audio);
    assert_eq!(track.codec_id, "A_AC3");
    assert_eq!(track.sample_rate, Some(48_000));
    assert_eq!(track.channels, Some(2));
    assert_eq!(track.codec_private, None);
    // 16 кадров × 1536 сэмплов: постоянная длина кадра даёт точную длительность.
    let expected = frame_start(FIXTURE_FRAMES as u64);
    assert_eq!(demuxer.duration(), Some(expected));
    assert_eq!(track.duration, Some(expected));
    assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
}

#[test]
fn eac3_codec_id_and_channels_come_from_the_bitstream() {
    let demuxer = open_direct(seekable_input(SURROUND_EAC3));
    let track = &demuxer.tracks()[0];
    assert_eq!(track.codec_id, "A_EAC3");
    assert_eq!(track.channels, Some(6));
    assert_eq!(track.sample_rate, Some(48_000));
    assert_eq!(demuxer.duration(), Some(frame_start(FIXTURE_FRAMES as u64)));
}

/// Каждый пакет — ровно один syncframe фикстуры, PTS = накопленные сэмплы.
#[test]
fn packets_are_whole_syncframes_with_cumulative_pts() {
    for (stream, frame_bytes) in [
        (STEREO_AC3, STEREO_FRAME_BYTES),
        (SURROUND_EAC3, SURROUND_EAC3_FRAME_BYTES),
    ] {
        let mut demuxer = open_direct(seekable_input(stream));
        let packets = drain_packets(&mut demuxer);
        assert_eq!(packets.len(), FIXTURE_FRAMES);
        for (index, packet) in packets.iter().enumerate() {
            let start = index * frame_bytes;
            assert_eq!(packet.data.as_ref(), &stream[start..start + frame_bytes]);
            assert_eq!(packet.pts, frame_start(index as u64));
            assert_eq!(packet.duration, Some(frame_start(1)));
            assert_eq!(packet.byte_offset, Some(start as u64));
            assert_eq!(packet.kind, TrackKind::Audio);
            assert_eq!(packet.track_id, demuxer.tracks()[0].id);
            assert_eq!(packet.keyframe, PacketKeyframe::NotKeyframe);
            let track_pts = packet.track_pts.expect("track pts");
            assert_eq!(track_pts.units.get(), index as i64 * 1536);
        }
    }
}

/// 44.1 кГц: длина кадра чередуется, но PTS остаются точными, а оценка
/// длительности по байтам отличается от правды меньше чем на кадр.
#[test]
fn variable_frame_length_keeps_exact_pts_and_close_duration() {
    let mut demuxer = open_direct(seekable_input(SURROUND_AC3_44K1));
    let duration = demuxer.duration().expect("duration");
    let packets = drain_packets(&mut demuxer);
    let lengths: Vec<usize> = packets.iter().map(|packet| packet.data.len()).collect();
    assert_eq!(lengths, [1950, 1952, 1950]);
    let frame = Duration::from_nanos(1536 * 1_000_000_000 / 44_100);
    for (index, packet) in packets.iter().enumerate() {
        let expected = Duration::from_nanos(index as u64 * 1536 * 1_000_000_000 / 44_100);
        assert_eq!(packet.pts, expected);
    }
    let exact = Duration::from_nanos(3 * 1536 * 1_000_000_000 / 44_100);
    assert!(
        duration.abs_diff(exact) < frame,
        "{duration:?} vs {exact:?}"
    );
}

/// Последовательный поток: те же пакеты, но без длительности и без seek-а.
#[test]
fn byte_stream_input_plays_without_duration_or_seek() {
    let mut demuxer = open_direct(stream_input(STEREO_AC3));
    assert_eq!(demuxer.duration(), None);
    assert!(matches!(
        demuxer.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
    let packets = drain_packets(&mut demuxer);
    assert_eq!(packets.len(), FIXTURE_FRAMES);
    assert_eq!(packets[15].pts, frame_start(15));
}

/// Byte source без seek-а (HTTP без Range): длительность известна по размеру, seek — нет.
#[test]
fn forward_only_byte_source_has_duration_but_no_seek() {
    use demux_api::DemuxInput;

    use crate::support::MemoryByteSource;

    let input = DemuxInput::byte_source(Box::new(MemoryByteSource::forward_only(STEREO_AC3)));
    let mut demuxer = open_direct(input);
    assert_eq!(demuxer.duration(), Some(frame_start(FIXTURE_FRAMES as u64)));
    assert!(matches!(
        demuxer.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
    assert!(demuxer.seek(Duration::ZERO).is_err());
    assert_eq!(drain_packets(&mut demuxer).len(), FIXTURE_FRAMES);
}

/// AC-3 пониженной частоты (bsid 9 = половина `fscod`): те же байты кадров,
/// но 24 кГц — значит, каждый кадр длится вдвое дольше, и таймлайн/длительность
/// обязаны это учитывать (иначе звук играл бы вдвое быстрее картинки/таймлайна).
#[test]
fn half_rate_bsid_9_ac3_has_halved_sample_rate_and_doubled_frame_duration() {
    const BITSTREAM_ID_BYTE: usize = 5;
    const HALF_RATE_BITSTREAM_ID: u8 = 9;
    let mut half_rate = STEREO_AC3.to_vec();
    for frame in half_rate.as_chunks_mut::<STEREO_FRAME_BYTES>().0 {
        // bsid — старшие 5 бит байта 5, bsmod (младшие 3) сохраняется.
        frame[BITSTREAM_ID_BYTE] =
            (HALF_RATE_BITSTREAM_ID << 3) | (frame[BITSTREAM_ID_BYTE] & 0x07);
    }
    let mut demuxer = open_direct(seekable_input(&half_rate));
    let track = demuxer.tracks()[0].clone();
    assert_eq!(track.codec_id, "A_AC3");
    assert_eq!(track.sample_rate, Some(24_000));
    let frame_duration = Duration::from_millis(64);
    assert_eq!(
        demuxer.duration(),
        Some(frame_duration * FIXTURE_FRAMES as u32)
    );
    let packets = drain_packets(&mut demuxer);
    assert_eq!(packets.len(), FIXTURE_FRAMES);
    for (index, packet) in packets.iter().enumerate() {
        assert_eq!(packet.pts, frame_duration * index as u32);
        assert_eq!(packet.duration, Some(frame_duration));
    }
}
