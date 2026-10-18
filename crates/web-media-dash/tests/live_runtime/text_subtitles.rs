//! Субтитры живого MPD: после принятого обновления манифеста индекс получает новые сегменты.

use super::*;

fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(payload.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(payload);
    bytes
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![version];
    payload.extend_from_slice(&flags.to_be_bytes()[1..]);
    payload.extend_from_slice(body);
    mp4_box(kind, &payload)
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.iter().flatten().copied().collect()
}

/// Инициализация `stpp`-дорожки с таймскейлом 1000.
fn stpp_init() -> Vec<u8> {
    let mut tkhd = vec![0; 8];
    tkhd.extend_from_slice(&1_u32.to_be_bytes());
    let mut mdhd = vec![0; 8];
    mdhd.extend_from_slice(&1000_u32.to_be_bytes());
    mdhd.extend_from_slice(&[0; 8]);
    let mut stsd = 1_u32.to_be_bytes().to_vec();
    stsd.extend_from_slice(&mp4_box(b"stpp", &[0, 0, 0, 0, 0, 0, 0, 1]));
    let stbl = mp4_box(b"stbl", &full_box(b"stsd", 0, 0, &stsd));
    let mdia = mp4_box(
        b"mdia",
        &concat(&[full_box(b"mdhd", 0, 0, &mdhd), mp4_box(b"minf", &stbl)]),
    );
    let trak = mp4_box(b"trak", &concat(&[full_box(b"tkhd", 0, 3, &tkhd), mdia]));
    concat(&[mp4_box(b"ftyp", b"iso6\0\0\0\0"), mp4_box(b"moov", &trak)])
}

/// Фрагмент с одним сэмплом TTML.
fn stpp_fragment(tfdt: u64, duration_ms: u32, sample: &[u8]) -> Vec<u8> {
    let build = |data_offset: i32| {
        let tfhd = full_box(b"tfhd", 0, 0x2_0000, &1_u32.to_be_bytes());
        let tfdt = full_box(b"tfdt", 1, 0, &tfdt.to_be_bytes());
        let mut trun = 1_u32.to_be_bytes().to_vec();
        trun.extend_from_slice(&data_offset.to_be_bytes());
        trun.extend_from_slice(&duration_ms.to_be_bytes());
        trun.extend_from_slice(&u32::try_from(sample.len()).unwrap().to_be_bytes());
        mp4_box(
            b"moof",
            &concat(&[
                full_box(b"mfhd", 0, 0, &1_u32.to_be_bytes()),
                mp4_box(
                    b"traf",
                    &concat(&[tfhd, tfdt, full_box(b"trun", 0, 0x301, &trun)]),
                ),
            ]),
        )
    };
    let moof_len = build(0).len();
    concat(&[
        build(i32::try_from(moof_len + 8).unwrap()),
        mp4_box(b"mdat", sample),
    ])
}

fn ttml(begin: &str, end: &str, text: &str) -> String {
    format!(
        r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="{begin}" end="{end}">{text}</p></div></body></tt>"#
    )
}

/// Динамический MPD: аудио (для живой сессии) + текстовый набор `stpp` по SegmentTimeline.
fn manifest(publish_time_seconds: u8, minimum_update_period: &str, segment_repeat: u8) -> Vec<u8> {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
      profiles="urn:mpeg:dash:profile:isoff-live:2011,http://dashif.org/guidelines/dash-if-simple"
      availabilityStartTime="1970-01-01T00:00:00Z"
      publishTime="1970-01-01T00:00:{publish_time_seconds:02}Z"
      minimumUpdatePeriod="{minimum_update_period}" suggestedPresentationDelay="PT0.05S">
      <UTCTiming schemeIdUri="urn:mpeg:dash:utc:http-xsdate:2014" value="clock"/>
      <Period id="p0" start="PT0S">
        <AdaptationSet contentType="audio" mimeType="audio/webm" codecs="opus">
          <Representation id="audio">
            <SegmentTemplate timescale="1000" initialization="init.webm" media="$Time$.webm">
              <SegmentTimeline><S t="0" d="200" r="{segment_repeat}"/></SegmentTimeline>
            </SegmentTemplate>
          </Representation>
        </AdaptationSet>
        <AdaptationSet mimeType="application/mp4" codecs="stpp" lang="en">
          <Label>Live English</Label>
          <SegmentTemplate timescale="1000" initialization="t-init.mp4" media="t-$Time$.m4s">
            <SegmentTimeline><S t="0" d="200" r="{segment_repeat}"/></SegmentTimeline>
          </SegmentTemplate>
          <Representation id="text"/>
        </AdaptationSet>
      </Period>
    </MPD>"#
    )
    .into_bytes()
}

#[test]
fn live_text_index_follows_accepted_manifest_refreshes_and_segments_decode_on_the_session_timeline()
{
    let first = ttml("00:00:00.050", "00:00:00.150", "first");
    let second = ttml("00:00:00.250", "00:00:00.350", "second");
    let server = HermeticDashServer::start_with_refresh(
        HashMap::from([
            ("/live.mpd", manifest(1, "PT0.25S", 0)),
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
            ("/t-init.mp4", stpp_init()),
            ("/t-0.m4s", stpp_fragment(0, 200, first.as_bytes())),
            ("/t-200.m4s", stpp_fragment(200, 200, second.as_bytes())),
        ]),
        RefreshManifestResponse {
            path: "/live.mpd",
            body: manifest(2, "PT60S", 1),
        },
    );
    let manifest_target = server.target("/live.mpd");
    let generation = SourceGeneration::new(9);
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
                NonZeroU64::new(9).expect("port generation"),
            ),
            initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
            endpoint_refresh: endpoint_refresh.clone(),
        },
        &cancellation,
    );
    let text = opened
        .text_subtitles()
        .expect("the live MPD declares a text track");
    let tracks = text.tracks();
    assert_eq!(tracks.len(), 1);
    assert_eq!(&*tracks[0].name, "Live English");
    let (demuxer, seek_handle, timeline_port) = opened.into_parts();

    let track = tracks[0].id;
    let index = text
        .load_index(track, &cancellation)
        .expect("initial index");
    assert_eq!(
        index.segments.len(),
        1,
        "only the first segment is announced"
    );
    assert!(
        matches!(index.refresh, web_media_dash::DashTextRefresh::After(period) if period >= Duration::from_millis(250)),
        "a live index asks to be re-read"
    );

    // Принятое обновление манифеста добавляет второй сегмент и в индекс субтитров.
    server.enable_refreshed_manifest();
    server.wait_for_refreshed_manifest_response();
    let deadline = Instant::now() + TEST_TIMEOUT;
    let index = loop {
        let index = text
            .load_index(track, &cancellation)
            .expect("refreshed index");
        if index.segments.len() == 2 {
            break index;
        }
        assert!(Instant::now() < deadline, "new text segment never appeared");
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(index.segments[0].start, Duration::ZERO);
    assert_eq!(index.segments[1].start, Duration::from_millis(200));

    // Документ нового сегмента скачивается и уже сдвинут на шкалу сессии (здесь сдвиг нулевой:
    // PTO = 0, начало Period = 0, начало сессии = самый ранний сегмент = 0).
    let limit = NonZeroUsize::new(64 * 1_024).unwrap();
    let fetched = text
        .fetch_segment(&index.segments[1].handle, limit, &cancellation)
        .expect("second segment decodes");
    assert_eq!(fetched.player_shift_micros, 0);
    assert_eq!(fetched.documents.len(), 1);
    assert_eq!(fetched.documents[0].bytes, second.as_bytes());

    cancellation.cancel();
    // Субтитры держат общее состояние сессии: отпускаем их до проверки закрытия.
    drop(text);
    drop(demuxer);
    drop(seek_handle);
    drop(timeline_port);
    wait_for_refresh_shutdown(&endpoint_refresh);
    wait_for_request_quiescence(&server);
}
