//! Новый publishTime без media suffix не завершает live playback.

use super::*;

#[test]
fn newer_manifest_without_suffix_keeps_live_waiting_without_replay() {
    let initial_manifest = dynamic_audio_manifest(DynamicAudioManifestFixture {
        publish_time_seconds: 1,
        minimum_update_period: "PT0.25S",
        segment_repeat: 0,
    });
    let refreshed_manifest = dynamic_audio_manifest(DynamicAudioManifestFixture {
        publish_time_seconds: 2,
        minimum_update_period: "PT0.01S",
        segment_repeat: 0,
    });
    let server = HermeticDashServer::start_with_refresh(
        HashMap::from([
            ("/live.mpd", initial_manifest),
            ("/clock", b"1970-01-01T00:00:02Z\n".to_vec()),
            (
                "/init.webm",
                decode_base64(include_str!("../fixtures/audio-webm-init.base64")),
            ),
            (
                "/0.webm",
                decode_base64(include_str!("../fixtures/audio-webm-one.base64")),
            ),
            (
                "/200.webm",
                decode_base64(include_str!("../fixtures/audio-webm-two.base64")),
            ),
        ]),
        RefreshManifestResponse {
            path: "/live.mpd",
            body: refreshed_manifest,
        },
    );
    let manifest_target = server.target("/live.mpd");
    let generation = SourceGeneration::new(11);
    let cancellation = CancellationToken::new();
    let endpoint_refresh = Arc::new(RejectingEndpointRefresh::new());
    let opened = prepare_dash_live_with_deadline(
        DashLiveOpenRequest {
            http: Box::new(adaptive_context(
                &manifest_target,
                cancellation.clone(),
                generation,
            )),
            generation,
            manifest: manifest_input(manifest_target),
            selection: audio_selection(),
            demux_registry: demux_registry(),
            policy: open_policy(),
            wall_clock: Arc::new(FixedWallClock {
                now: DashUtcTimestamp::from_unix_nanoseconds(2_000_000_000),
            }),
            timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
                NonZeroU64::new(11).expect("DASH continuation timeline port generation"),
            ),
            initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
            endpoint_refresh: endpoint_refresh.clone(),
        },
        &cancellation,
    );
    let (mut demuxer, seek_handle, timeline_port) = opened.into_parts();

    // Дочитываем настоящий initial Opus fragment до нейтрального live waiting.
    let first = observe_until_packet_at_or_after(&mut demuxer, Duration::from_millis(150)).packet;
    assert_eq!(first.kind, TrackKind::Audio);
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "initial fragment must finish");
        match demuxer.next_event().expect("consume initial fragment") {
            DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => break,
            event => panic!("unexpected initial event: {event:?}"),
        }
    }
    let consumed_requests = server
        .requested_paths()
        .iter()
        .filter(|p| *p == "/0.webm")
        .count();
    server.enable_refreshed_manifest();
    server.wait_for_two_refreshed_responses();
    // Второй fetch начинается только после commit первого accepted MPD в
    // последовательном refresh owner-е. Одна отправка HTTP body этого не доказывает.
    assert!(matches!(
        demuxer.next_event().expect("no-suffix continuation"),
        DemuxReadEvent::TemporarilyUnavailable(_)
    ));
    assert_eq!(
        server
            .requested_paths()
            .iter()
            .filter(|p| *p == "/0.webm")
            .count(),
        consumed_requests
    );
    assert!(!server.requested_paths().iter().any(|p| p == "/200.webm"));

    cancellation.cancel();
    drop(demuxer);
    drop(seek_handle);
    drop(timeline_port);
    wait_for_refresh_shutdown(&endpoint_refresh);
    wait_for_request_quiescence(&server);
}

impl HermeticDashServer {
    /// Две последовательные newer responses доказывают завершение первого commit.
    fn wait_for_two_refreshed_responses(&self) {
        let deadline = Instant::now() + TEST_TIMEOUT;
        let (lock, changed) = &*self.served_requests;
        let mut observed = lock.lock().expect("served request log");
        while observed.refreshed_manifest_responses < 2 {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("second refresh deadline");
            let (next, _) = changed
                .wait_timeout(observed, remaining)
                .expect("second refresh rendezvous");
            observed = next;
        }
    }
}
