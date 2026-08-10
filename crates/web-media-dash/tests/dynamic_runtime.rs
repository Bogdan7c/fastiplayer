use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{
    DashContainer, DashMediaKind, DashMpdLimits, DashMpdParseRequest, DashUtcTimestamp,
    parse_dynamic_dash_mpd,
};
use source_core::HttpRequestTarget;
use web_media_dash::{
    DashLiveRefreshError, DashLiveRefreshOutcome, DashPresentationSelection,
    DashRepresentationEvidence, DashSynchronizedClock, DashWallClock, build_dash_live_snapshot,
    refresh_dash_live_snapshot,
};

/// Deterministic injected local wall clock.
struct FakeClock {
    now: Mutex<DashUtcTimestamp>,
}

impl FakeClock {
    /// Создаёт clock с заданным timestamp.
    fn new(now: DashUtcTimestamp) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    /// Продвигает clock без sleep.
    fn set(&self, now: DashUtcTimestamp) {
        *self.now.lock().expect("test clock mutex") = now;
    }
}

impl DashWallClock for FakeClock {
    /// Возвращает test-owned wall time.
    fn now_utc(&self) -> DashUtcTimestamp {
        *self.now.lock().expect("test clock mutex")
    }
}

/// Explicit parser budgets.
fn xml_budgets() -> XmlBudgets {
    XmlBudgets::builder()
        .maximum_document_bytes(64 * 1024)
        .maximum_depth(32)
        .maximum_tokens(2_048)
        .maximum_attributes_per_element(32)
        .maximum_attribute_count(1_024)
        .maximum_attribute_bytes(32 * 1024)
        .maximum_namespace_declarations_per_element(8)
        .maximum_namespace_declaration_count(32)
        .maximum_namespace_bytes(4 * 1024)
        .maximum_text_bytes(32 * 1024)
        .build()
        .expect("test XML budget")
}

/// Explicit schema caps.
fn limits() -> DashMpdLimits {
    DashMpdLimits {
        maximum_periods: 8,
        maximum_adaptation_sets_per_period: 8,
        maximum_representations_per_adaptation_set: 8,
        maximum_segments_per_list: 8,
        maximum_timeline_entries: 32,
        maximum_schema_string_bytes: 4 * 1024,
    }
}

/// Создаёт aligned selected A/V fixture.
fn fixture(publish_time: &str, video_ato: &str, extra_video_representation: &str) -> String {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
          availabilityStartTime="2026-07-24T10:00:00Z"
          publishTime="{publish_time}" minimumUpdatePeriod="PT2S"
          timeShiftBufferDepth="PT30S" suggestedPresentationDelay="PT6S">
          <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014"
            value="2026-07-24T10:01:00Z"/>
          <Period id="p0" start="PT0S" duration="PT120S">
            <AdaptationSet id="v" mimeType="video/mp4" codecs="avc1.4d401f">
              <SegmentTemplate timescale="1" initialization="v-init.mp4"
                media="v-$Time$.m4s" availabilityTimeOffset="{video_ato}">
                <SegmentTimeline><S t="0" d="2" r="59"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="video"/>{extra_video_representation}
            </AdaptationSet>
            <AdaptationSet id="a" mimeType="audio/mp4" codecs="mp4a.40.2">
              <SegmentTemplate timescale="1" initialization="a-init.mp4"
                media="a-$Time$.m4s">
                <SegmentTimeline><S t="0" d="2" r="59"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="audio"/>
            </AdaptationSet>
          </Period>
        </MPD>"#
    )
}

/// Парсит fixture.
fn parse(document: &str) -> dash_mpd_core::DashDynamicMpd {
    parse_dynamic_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: limits(),
    })
    .expect("dynamic fixture")
}

/// Извлекает direct UTC только из fixtures, которые явно объявляют этот scheme.
fn direct_utc(mpd: &dash_mpd_core::DashDynamicMpd) -> DashUtcTimestamp {
    mpd.direct_utc_time()
        .expect("test fixture использует direct UTC")
}

/// Exact selected component evidence.
fn selection() -> DashPresentationSelection {
    DashPresentationSelection::Separate {
        video: DashRepresentationEvidence {
            media_kind: DashMediaKind::Video,
            container: DashContainer::IsoBmff,
            representation_id: Some("video".to_owned()),
            codecs: Some("avc1.4d401f".to_owned()),
            bandwidth: None,
            dimensions: None,
        },
        audio: DashRepresentationEvidence {
            media_kind: DashMediaKind::Audio,
            container: DashContainer::IsoBmff,
            representation_id: Some("audio".to_owned()),
            codecs: Some("mp4a.40.2".to_owned()),
            bandwidth: None,
            dimensions: None,
        },
    }
}

/// Строит snapshot с clock, synchronized напрямую с UTCTiming.
fn snapshot(document: &str, clock: Arc<FakeClock>) -> web_media_dash::DashLiveSnapshot {
    let mpd = parse(document);
    let direct = direct_utc(&mpd);
    let synchronized = DashSynchronizedClock::from_direct_utc(clock, direct, direct, direct)
        .expect("zero clock offset");
    build_dash_live_snapshot(
        mpd,
        &HttpRequestTarget::parse_exact("https://media.invalid/live/manifest.mpd")
            .expect("test URL"),
        &selection(),
        NonZeroUsize::new(512).expect("non-zero"),
        &synchronized,
    )
    .expect("live snapshot")
}

#[test]
fn synchronized_clock_spd_tsb_and_av_intersection_define_manifest_cap() {
    let direct = direct_utc(&parse(&fixture("2026-07-24T10:01:00Z", "0", "")));
    let clock = Arc::new(FakeClock::new(DashUtcTimestamp::from_unix_nanoseconds(
        direct.unix_nanoseconds() + 40_000_000_000,
    )));
    let initial_snapshot = snapshot(
        &fixture("2026-07-24T10:01:00Z", "0", ""),
        Arc::clone(&clock),
    );
    assert_eq!(
        initial_snapshot
            .availability
            .manifest_range
            .start
            .as_duration(),
        std::time::Duration::from_secs(70)
    );
    assert_eq!(
        initial_snapshot.availability.live_edge.as_duration(),
        std::time::Duration::from_secs(94)
    );

    clock.set(DashUtcTimestamp::from_unix_nanoseconds(
        direct.unix_nanoseconds() + 44_000_000_000,
    ));
    let slid = snapshot(&fixture("2026-07-24T10:01:02Z", "-10", ""), clock);
    assert_eq!(
        slid.availability.live_edge.as_duration(),
        std::time::Duration::from_secs(94)
    );

    let skewed_local = Arc::new(FakeClock::new(DashUtcTimestamp::from_unix_nanoseconds(
        direct.unix_nanoseconds() + 35_000_000_000,
    )));
    let synchronized = DashSynchronizedClock::from_direct_utc(
        skewed_local.clone(),
        DashUtcTimestamp::from_unix_nanoseconds(direct.unix_nanoseconds() - 5_000_000_000),
        DashUtcTimestamp::from_unix_nanoseconds(direct.unix_nanoseconds() - 5_000_000_000),
        direct,
    )
    .expect("direct UTC compensates local skew");
    assert_eq!(
        synchronized.now_utc().expect("synchronized now"),
        DashUtcTimestamp::from_unix_nanoseconds(direct.unix_nanoseconds() + 40_000_000_000)
    );
}

#[test]
fn different_audio_video_pto_values_map_to_the_same_presentation_availability() {
    let mut document = fixture("2026-07-24T10:01:00Z", "0", "");
    document = document.replacen(
        r#"<SegmentTemplate timescale="1" initialization="v-init.mp4""#,
        r#"<SegmentTemplate timescale="1" presentationTimeOffset="100" initialization="v-init.mp4""#,
        1,
    );
    document = document.replacen(
        r#"<SegmentTimeline><S t="0" d="2" r="59"/></SegmentTimeline>"#,
        r#"<SegmentTimeline><S t="100" d="2" r="59"/></SegmentTimeline>"#,
        1,
    );
    document = document.replacen(
        r#"<SegmentTemplate timescale="1" initialization="a-init.mp4""#,
        r#"<SegmentTemplate timescale="1" presentationTimeOffset="200" initialization="a-init.mp4""#,
        1,
    );
    document = document.replacen(
        r#"<SegmentTimeline><S t="0" d="2" r="59"/></SegmentTimeline>"#,
        r#"<SegmentTimeline><S t="200" d="2" r="59"/></SegmentTimeline>"#,
        1,
    );
    let direct = direct_utc(&parse(&document));
    let clock = Arc::new(FakeClock::new(DashUtcTimestamp::from_unix_nanoseconds(
        direct.unix_nanoseconds() + 40_000_000_000,
    )));
    let pto_snapshot = snapshot(&document, clock);

    assert_eq!(
        pto_snapshot.availability.manifest_range.start.as_duration(),
        std::time::Duration::from_secs(70)
    );
    assert_eq!(
        pto_snapshot.availability.live_edge.as_duration(),
        std::time::Duration::from_secs(94)
    );
}

#[test]
fn multi_period_live_keeps_global_continuity_with_new_raw_pto_per_period() {
    let document = fixture("2026-07-24T10:01:00Z", "0", "")
        .replace(r#"duration="PT120S""#, r#"duration="PT60S""#)
        .replace(r#"d="2" r="59""#, r#"d="2" r="29""#)
        .replace(
            "</Period>\n        </MPD>",
            r#"</Period>
          <Period id="p1" start="PT60S" duration="PT60S">
            <AdaptationSet id="v" mimeType="video/mp4" codecs="avc1.4d401f">
              <SegmentTemplate timescale="1" presentationTimeOffset="500"
                initialization="v2-init.mp4" media="v2-$Time$.m4s">
                <SegmentTimeline><S t="500" d="2" r="29"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="video"/>
            </AdaptationSet>
            <AdaptationSet id="a" mimeType="audio/mp4" codecs="mp4a.40.2">
              <SegmentTemplate timescale="1" presentationTimeOffset="700"
                initialization="a2-init.mp4" media="a2-$Time$.m4s">
                <SegmentTimeline><S t="700" d="2" r="29"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="audio"/>
            </AdaptationSet>
          </Period>
        </MPD>"#,
        );
    let direct = direct_utc(&parse(&document));
    let clock = Arc::new(FakeClock::new(DashUtcTimestamp::from_unix_nanoseconds(
        direct.unix_nanoseconds() + 40_000_000_000,
    )));
    let multi_period = snapshot(&document, clock);

    assert_eq!(
        multi_period.availability.manifest_range.start.as_duration(),
        std::time::Duration::from_secs(70)
    );
    assert_eq!(
        multi_period.availability.live_edge.as_duration(),
        std::time::Duration::from_secs(94)
    );
}

#[test]
fn equal_older_newer_publish_order_and_continuity_are_atomic() {
    let direct = direct_utc(&parse(&fixture("2026-07-24T10:01:00Z", "0", "")));
    let clock = Arc::new(FakeClock::new(DashUtcTimestamp::from_unix_nanoseconds(
        direct.unix_nanoseconds() + 40_000_000_000,
    )));
    let mut current = snapshot(
        &fixture("2026-07-24T10:01:00Z", "0", ""),
        Arc::clone(&clock),
    );
    let original_publish = current.mpd.publish_time;

    let equal = snapshot(
        &fixture("2026-07-24T10:01:00Z", "0", ""),
        Arc::clone(&clock),
    );
    assert_eq!(
        refresh_dash_live_snapshot(&mut current, equal).expect("equal turn"),
        DashLiveRefreshOutcome::EqualUnchanged
    );
    assert_eq!(current.mpd.publish_time, original_publish);

    let older = snapshot(
        &fixture("2026-07-24T10:00:58Z", "0", ""),
        Arc::clone(&clock),
    );
    assert_eq!(
        refresh_dash_live_snapshot(&mut current, older).expect("stale ignored"),
        DashLiveRefreshOutcome::StaleIgnored
    );
    assert_eq!(current.mpd.publish_time, original_publish);

    let reordered_sibling = fixture(
        "2026-07-24T10:01:02Z",
        "0",
        r#"<Representation id="backup"/>"#,
    )
    .replace(
        r#"<Representation id="video"/><Representation id="backup"/>"#,
        r#"<Representation id="backup"/><Representation id="video"/>"#,
    );
    let reordered_sibling = snapshot(&reordered_sibling, Arc::clone(&clock));
    assert_eq!(
        refresh_dash_live_snapshot(&mut current, reordered_sibling)
            .expect("non-selected sibling reorder"),
        DashLiveRefreshOutcome::Replaced
    );

    let incompatible_document = fixture("2026-07-24T10:01:04Z", "0", "").replace(
        r#"<Representation id="video"/>"#,
        r#"<Representation id="video" width="1920" height="1080"/>"#,
    );
    let incompatible = snapshot(&incompatible_document, Arc::clone(&clock));
    assert!(matches!(
        refresh_dash_live_snapshot(&mut current, incompatible),
        Err(DashLiveRefreshError::Continuity)
    ));
    assert!(current.mpd.publish_time > original_publish);

    let newer = snapshot(&fixture("2026-07-24T10:01:04Z", "0", ""), clock);
    assert_eq!(
        refresh_dash_live_snapshot(&mut current, newer).expect("newer commit"),
        DashLiveRefreshOutcome::Replaced
    );
    assert!(current.mpd.publish_time > original_publish);
}
