//! Multi-epoch timeline evidence отделено от transport/crypto fixture matrix.

use super::*;

#[test]
fn ts_and_fmp4_publish_epochs_with_monotonic_timestamps() {
    let first_ts = Arc::new(muxed_ts(90_000));
    let second_ts = Arc::new(muxed_ts(90_000));
    let ts_server = TestServer::start(move |_, request| {
        if request.request_line.contains("/first.ts") {
            response("200 OK", &[], &first_ts)
        } else if request.request_line.contains("/second.ts") {
            response("200 OK", &[], &second_ts)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let ts_playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                       #EXTINF:1,\nfirst.ts\n#EXT-X-DISCONTINUITY\n\
                       #EXTINF:1,\nsecond.ts\n#EXT-X-ENDLIST\n";
    let ts_opened = prepare_hls_vod(inline_request(
        &ts_server,
        ts_playlist,
        HlsRequiredContainer::TransportStream,
    ))
    .expect("prepare discontinuous TS");
    assert_eq!(ts_opened.duration(), Duration::from_secs(2));
    let mut ts_demuxer = ts_opened.into_demuxer();
    assert_monotonic_epoch_events(
        collect_until_eos(&mut *ts_demuxer).expect("TS discontinuity events"),
    );

    let (initialization, first_media, _) = muxed_fmp4();
    let initialization = Arc::new(initialization);
    let first_media = Arc::new(first_media);
    let fmp4_server = TestServer::start(move |_, request| {
        if request.request_line.contains("/init.mp4") {
            response("200 OK", &[], &initialization)
        } else if request.request_line.contains("/first.m4s")
            || request.request_line.contains("/second.m4s")
        {
            response("200 OK", &[], &first_media)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let fmp4_playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                         #EXT-X-MAP:URI=\"init.mp4\"\n\
                         #EXTINF:1,\nfirst.m4s\n#EXT-X-DISCONTINUITY\n\
                         #EXTINF:1,\nsecond.m4s\n#EXT-X-ENDLIST\n";
    let fmp4_opened = prepare_hls_vod(inline_request(
        &fmp4_server,
        fmp4_playlist,
        HlsRequiredContainer::FragmentedMp4,
    ))
    .expect("prepare discontinuous fMP4");
    assert_eq!(fmp4_opened.duration(), Duration::from_secs(2));
    let mut fmp4_demuxer = fmp4_opened.into_demuxer();
    assert_monotonic_epoch_events(
        collect_until_eos(&mut *fmp4_demuxer).expect("fMP4 discontinuity events"),
    );
}

fn assert_monotonic_epoch_events(events: Vec<DemuxReadEvent>) {
    assert!(
        events
            .iter()
            .filter(|event| matches!(event, DemuxReadEvent::TracksChanged(_)))
            .count()
            >= 2,
        "initial and discontinuity track snapshots expected"
    );
    let timestamps = events
        .iter()
        .filter_map(|event| match event {
            DemuxReadEvent::Packet(packet) => Some(packet.pts),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!timestamps.is_empty());
    assert!(
        timestamps.windows(2).all(|pair| pair[0] <= pair[1]),
        "global packet timestamps must be monotonic across epochs"
    );
}
