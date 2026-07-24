use std::num::NonZeroUsize;
use std::sync::Arc;

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{
    DashContainer, DashMediaKind, DashMpdLimits, DashMpdParseRequest, DashUtcTimestamp,
    parse_dynamic_dash_mpd,
};
use source_core::HttpRequestTarget;

use super::{
    DashLiveClockError, DashLiveProfileExclusion, DashLiveRefreshError, DashLiveRefreshOutcome,
    DashSynchronizedClock, DashWallClock, build_dash_live_snapshot, map_dynamic_plan_error,
    replace_dash_live_endpoint_snapshot,
};
use crate::{DashPlanError, DashPresentationSelection, DashRepresentationEvidence};

/// Неизменяемый local clock для exact offset assertions.
struct FixedClock {
    now: DashUtcTimestamp,
}

impl DashWallClock for FixedClock {
    fn now_utc(&self) -> DashUtcTimestamp {
        self.now
    }
}

/// Строит minimal endpoint snapshot для commit atomicity tests.
fn endpoint_snapshot(availability_start_time: &str, publish_time: &str) -> super::DashLiveSnapshot {
    let document = format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
          availabilityStartTime="{availability_start_time}"
          publishTime="{publish_time}" minimumUpdatePeriod="PT2S"
          timeShiftBufferDepth="PT30S" suggestedPresentationDelay="PT6S">
          <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014"
            value="2026-07-24T10:01:00Z"/>
          <Period id="p0" start="PT0S" duration="PT120S">
            <AdaptationSet id="v" mimeType="video/mp4" codecs="avc1.4d401f">
              <SegmentTemplate timescale="1" initialization="init.mp4"
                media="v-$Time$.m4s">
                <SegmentTimeline><S t="0" d="2" r="59"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="video"/>
            </AdaptationSet>
          </Period>
        </MPD>"#
    );
    let budgets = XmlBudgets::builder()
        .maximum_document_bytes(32 * 1024)
        .maximum_depth(32)
        .maximum_tokens(512)
        .maximum_attributes_per_element(32)
        .maximum_attribute_count(256)
        .maximum_attribute_bytes(16 * 1024)
        .maximum_namespace_declarations_per_element(8)
        .maximum_namespace_declaration_count(16)
        .maximum_namespace_bytes(2 * 1024)
        .maximum_text_bytes(16 * 1024)
        .build()
        .expect("test XML budgets");
    let mpd = parse_dynamic_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: budgets,
        limits: DashMpdLimits {
            maximum_periods: 2,
            maximum_adaptation_sets_per_period: 2,
            maximum_representations_per_adaptation_set: 2,
            maximum_segments_per_list: 8,
            maximum_timeline_entries: 8,
            maximum_schema_string_bytes: 2 * 1024,
        },
    })
    .expect("strict endpoint fixture");
    let local: Arc<dyn DashWallClock> = Arc::new(FixedClock {
        now: mpd.direct_utc_time,
    });
    let clock = DashSynchronizedClock::from_direct_utc(
        local,
        mpd.direct_utc_time,
        mpd.direct_utc_time,
        mpd.direct_utc_time,
    )
    .expect("zero-offset endpoint clock");
    build_dash_live_snapshot(
        mpd,
        &HttpRequestTarget::parse_exact("https://media.invalid/live.mpd").expect("test target"),
        &DashPresentationSelection::Single {
            main: DashRepresentationEvidence {
                media_kind: DashMediaKind::Video,
                container: DashContainer::IsoBmff,
                representation_id: Some("video".to_owned()),
                codecs: Some("avc1.4d401f".to_owned()),
                bandwidth: None,
                dimensions: None,
            },
        },
        NonZeroUsize::new(128).expect("non-zero"),
        &clock,
    )
    .expect("endpoint snapshot")
}

#[test]
fn direct_utc_uses_after_minus_floor_half_rtt_for_odd_nanoseconds() {
    let local: Arc<dyn DashWallClock> = Arc::new(FixedClock {
        now: DashUtcTimestamp::from_unix_nanoseconds(105),
    });
    let synchronized = DashSynchronizedClock::from_direct_utc(
        local,
        DashUtcTimestamp::from_unix_nanoseconds(100),
        DashUtcTimestamp::from_unix_nanoseconds(105),
        DashUtcTimestamp::from_unix_nanoseconds(1_000),
    )
    .expect("forward local interval must synchronize");

    // Midpoint policy: 105 - floor(5 / 2) = 103, значит now=105 даёт 1002.
    assert_eq!(
        synchronized
            .now_utc()
            .expect("synchronized now must remain in range")
            .unix_nanoseconds(),
        1_002
    );
}

#[test]
fn local_clock_regression_is_typed_and_fail_closed() {
    let local: Arc<dyn DashWallClock> = Arc::new(FixedClock {
        now: DashUtcTimestamp::from_unix_nanoseconds(99),
    });
    let result = DashSynchronizedClock::from_direct_utc(
        local,
        DashUtcTimestamp::from_unix_nanoseconds(100),
        DashUtcTimestamp::from_unix_nanoseconds(99),
        DashUtcTimestamp::from_unix_nanoseconds(1_000),
    );

    assert!(matches!(result, Err(DashLiveClockError::ClockRegression)));
}

#[test]
fn direct_utc_midpoint_arithmetic_overflow_is_typed() {
    let local: Arc<dyn DashWallClock> = Arc::new(FixedClock {
        now: DashUtcTimestamp::from_unix_nanoseconds(i128::MAX),
    });
    let result = DashSynchronizedClock::from_direct_utc(
        local,
        DashUtcTimestamp::from_unix_nanoseconds(i128::MAX),
        DashUtcTimestamp::from_unix_nanoseconds(i128::MAX),
        DashUtcTimestamp::from_unix_nanoseconds(i128::MIN),
    );

    assert!(matches!(result, Err(DashLiveClockError::Overflow)));
}

#[test]
fn unsupported_timeline_models_remain_profile_exclusions() {
    for (plan_error, expected) in [
        (
            DashPlanError::TimelineGap,
            DashLiveProfileExclusion::TimelineGap,
        ),
        (
            DashPlanError::TimelineOverlap,
            DashLiveProfileExclusion::TimelineOverlap,
        ),
        (
            DashPlanError::SegmentCrossesPeriodBoundary,
            DashLiveProfileExclusion::SegmentCrossesPeriodBoundary,
        ),
    ] {
        assert!(matches!(
            map_dynamic_plan_error(plan_error),
            DashLiveRefreshError::ProfileExcluded(actual) if actual == expected
        ));
    }
}

#[test]
fn stale_or_incompatible_endpoint_snapshot_never_mutates_authoritative_snapshot() {
    let mut current = endpoint_snapshot("2026-07-24T10:00:00Z", "2026-07-24T10:01:00Z");
    let original_publish = current.mpd.publish_time;
    let original_availability = current.availability.clone();
    let stale = endpoint_snapshot("2026-07-24T10:00:00Z", "2026-07-24T10:00:58Z");

    assert_eq!(
        replace_dash_live_endpoint_snapshot(&mut current, stale)
            .expect("stale endpoint is classified"),
        DashLiveRefreshOutcome::StaleIgnored
    );
    assert_eq!(current.mpd.publish_time, original_publish);
    assert_eq!(current.availability, original_availability);

    let incompatible = endpoint_snapshot("2026-07-24T09:59:00Z", "2026-07-24T10:01:02Z");
    assert!(matches!(
        replace_dash_live_endpoint_snapshot(&mut current, incompatible),
        Err(DashLiveRefreshError::Continuity)
    ));
    assert_eq!(current.mpd.publish_time, original_publish);
    assert_eq!(current.availability, original_availability);
}
