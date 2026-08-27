use std::sync::Arc;
use std::time::{Duration, Instant};

use demux_api::{
    ProgressiveAsyncSeekLimits, ProgressiveAsyncSeekOutcome, ProgressiveSeekFence,
    ProgressiveSeekRequestId,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, PacketKeyframe, TrackKind};

use super::{
    NonZeroUsize, SEGMENT_SECONDS, TEST_TIMEOUT, TestServer, initial_track_signature,
    long_muxed_ts_segment, next_ready_event, post_target_request, prepare_hls_vod_receipted,
    response, wait_for_receipt,
};

#[test]
fn final_receipt_lands_on_packet_derived_anchor_across_discontinuity() {
    let segments = Arc::new(
        (0..3_u64)
            .map(|segment_index| {
                long_muxed_ts_segment(
                    segment_index * SEGMENT_SECONDS * 90_000,
                    SEGMENT_SECONDS,
                )
            })
            .collect::<Vec<_>>(),
    );
    let server_segments = Arc::clone(&segments);
    let server = TestServer::start(move |_, request| {
        server_segments
            .iter()
            .enumerate()
            .find_map(|(segment_index, segment)| {
                request
                    .request_line
                    .contains(&format!("/diagnostic-{segment_index}.ts"))
                    .then(|| response("200 OK", &[], segment))
            })
            .unwrap_or_else(|| response("404 Not Found", &[], b""))
    });
    let playlist = "#EXTM3U\n\
#EXT-X-TARGETDURATION:10\n\
#EXT-X-MEDIA-SEQUENCE:50\n\
#EXT-X-DISCONTINUITY-SEQUENCE:4\n\
#EXTINF:10,\n\
diagnostic-0.ts\n\
#EXTINF:10,\n\
diagnostic-1.ts\n\
#EXT-X-DISCONTINUITY\n\
#EXTINF:10,\n\
diagnostic-2.ts\n\
#EXT-X-ENDLIST\n";
    let opened = prepare_hls_vod_receipted(
        post_target_request(&server, playlist),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("prepare diagnostic HLS VOD");
    let seek_handle = opened
        .async_seek_handle()
        .expect("diagnostic receipted seek handle");
    let mut demuxer = opened.into_demuxer();
    initial_track_signature(&mut *demuxer);

    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(1),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(15)),
        )
        .expect("enqueue discontinuity seek");
    let receipt = wait_for_receipt(&seek_handle);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("diagnostic seek must succeed: {receipt:?}");
    };
    assert_eq!(result.actual_position.as_duration(), Duration::from_secs(20));

    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match next_ready_event(&mut *demuxer).expect("post-seek diagnostic event") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
                assert_eq!(packet.pts, Duration::from_secs(20));
                break;
            }
            DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::Packet(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("HLS ended before diagnostic landing packet"),
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
        assert!(Instant::now() < deadline, "diagnostic landing timed out");
    }

}
