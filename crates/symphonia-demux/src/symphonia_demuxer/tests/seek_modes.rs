//! Выбор режима Symphonia seek и результат seek.

use super::*;

#[test]
fn accurate_demux_seek_uses_symphonia_accurate_mode() {
    assert_symphonia_seek_mode(
        DemuxSeekRequest::accurate(Duration::from_millis(500)),
        SeekMode::Accurate,
    );
}

#[test]
fn decode_point_before_demux_seek_uses_symphonia_accurate_mode() {
    assert_symphonia_seek_mode(
        DemuxSeekRequest::decode_point_before(Duration::from_millis(500)),
        SeekMode::Accurate,
    );
}

#[test]
fn preview_demux_seek_uses_symphonia_coarse_mode() {
    assert_symphonia_seek_mode(
        DemuxSeekRequest::preview(Duration::from_millis(500)),
        SeekMode::Coarse,
    );
}

#[test]
fn preview_seek_clears_decode_point_prebuffer_without_verification() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_keyframe_packet(1, 400))],
            vec![Ok(small_vp9_keyframe_packet(1, 11_000))],
        ])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "preview-clears-prebuffer",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("первый seek должен создать verification prebuffer");
    demuxer
        .seek_with_request(DemuxSeekRequest::preview(Duration::from_secs(10)))
        .expect("preview-mode seek не должен запускать DecodePointBefore verification");
    let packet_event = demuxer
        .next_event()
        .expect("preview packet должен читаться после seek");
    let DemuxReadEvent::Packet(packet) = packet_event else {
        panic!("preview-mode seek должен вернуть packet, получено {packet_event:?}");
    };

    assert_eq!(packet.pts, Duration::from_secs(11));
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .as_slice(),
        &[SeekMode::Accurate, SeekMode::Coarse]
    );
}

#[test]
fn seek_result_uses_selected_video_track_timestamp_when_audio_track_is_first() {
    let reader = FakeFormatReader::new(
        vec![
            aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000)),
            vp9_video_track(1),
        ],
        Vec::new(),
    );
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "selected-video-track-timestamp",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::preview(Duration::from_secs(10)))
        .expect("preview-mode seek должен использовать selected video track");

    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("seek result должен сохранить raw timestamp")
            .track_id,
        TrackId::new(1)
    );
}
