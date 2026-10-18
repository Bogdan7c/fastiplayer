//! Тесты дорожек, индекса и привязки времени субтитров DASH (без сети).

use std::num::NonZeroUsize;
use std::time::Duration;

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{DashMpd, DashMpdLimits, DashMpdParseRequest, parse_dash_mpd};
use source_core::HttpRequestTarget;

use super::fetch::index_segments;
use super::tracks::{build_text_plan_set, describe_tracks};
use crate::plan::DashTextTimeBasis;

pub(super) fn parse(document: &str) -> DashMpd {
    parse_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: XmlBudgets::builder()
            .maximum_document_bytes(64 * 1024)
            .maximum_depth(32)
            .maximum_tokens(4_096)
            .maximum_attributes_per_element(32)
            .maximum_attribute_count(2_048)
            .maximum_attribute_bytes(32 * 1024)
            .maximum_namespace_declarations_per_element(8)
            .maximum_namespace_declaration_count(32)
            .maximum_namespace_bytes(4 * 1024)
            .maximum_text_bytes(32 * 1024)
            .build()
            .unwrap(),
        limits: DashMpdLimits {
            maximum_periods: 8,
            maximum_adaptation_sets_per_period: 16,
            maximum_representations_per_adaptation_set: 8,
            maximum_segments_per_list: 64,
            maximum_timeline_entries: 64,
            maximum_schema_string_bytes: 4096,
        },
    })
    .unwrap()
}

fn base() -> HttpRequestTarget {
    HttpRequestTarget::parse_exact("https://cdn.example.test/vod/manifest.mpd").unwrap()
}

fn limit() -> NonZeroUsize {
    NonZeroUsize::new(64).unwrap()
}

/// Два Period по 10 с: видео и текстовые наборы, перечисленные в `text` (по Period).
pub(super) fn mpd_with_text(first_period_text: &str, second_period_text: &str) -> DashMpd {
    parse(&format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT20S">
          <Period id="a" start="PT0S" duration="PT10S">
            <AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
              <Representation id="v"><BaseURL>v.mp4</BaseURL></Representation></AdaptationSet>
            {first_period_text}
          </Period>
          <Period id="b" start="PT10S" duration="PT10S">
            <AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
              <Representation id="v"><BaseURL>v2.mp4</BaseURL></Representation></AdaptationSet>
            {second_period_text}
          </Period>
        </MPD>"#
    ))
}

#[test]
fn same_language_and_label_across_periods_form_one_track_with_two_period_plans() {
    let text = r#"<AdaptationSet mimeType="text/vtt" lang="en">
        <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/><Label>English</Label>
        <Representation id="t"><BaseURL>en.vtt</BaseURL></Representation></AdaptationSet>"#;
    let plan = build_text_plan_set(&mpd_with_text(text, text), &base(), limit());
    assert_eq!(plan.tracks.len(), 1);
    let periods = &plan.tracks[0].periods;
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[0].period_start, Duration::ZERO);
    assert_eq!(periods[1].period_start, Duration::from_secs(10));
    assert_eq!(periods[1].basis, DashTextTimeBasis::PeriodTimeline);
}

#[test]
fn tracks_get_label_or_role_names_unique_names_and_default_flags() {
    let text = r#"
      <AdaptationSet mimeType="text/vtt" lang="en"><Label>English SDH</Label>
        <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
        <Representation id="a"><BaseURL>a.vtt</BaseURL></Representation></AdaptationSet>
      <AdaptationSet mimeType="text/vtt" lang="en">
        <Role schemeIdUri="urn:mpeg:dash:role:2011" value="forced-subtitle"/>
        <Representation id="b"><BaseURL>b.vtt</BaseURL></Representation></AdaptationSet>
      <AdaptationSet mimeType="text/vtt" lang="fr">
        <Accessibility schemeIdUri="urn:mpeg:dash:role:2011" value="caption"/>
        <Representation id="c"><BaseURL>c.vtt</BaseURL></Representation></AdaptationSet>
      <AdaptationSet mimeType="text/vtt" lang="de">
        <Representation id="d"><BaseURL>d1.vtt</BaseURL></Representation></AdaptationSet>
      <AdaptationSet mimeType="text/vtt" lang="de">
        <Representation id="e"><BaseURL>d2.vtt</BaseURL></Representation></AdaptationSet>"#;
    let plan = build_text_plan_set(&mpd_with_text(text, ""), &base(), limit());
    let described = describe_tracks(&plan);
    let summary: Vec<(Option<&str>, &str, bool, bool)> = described
        .iter()
        .map(|track| {
            (
                track.language.as_deref(),
                track.name.as_str(),
                track.forced,
                track.default,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (Some("en"), "English SDH", false, true),
            (Some("en"), "Forced", true, false),
            (Some("fr"), "Captions", false, false),
            (Some("de"), "Subtitles", false, false),
            (Some("de"), "Subtitles (2)", false, false),
        ]
    );
}

#[test]
fn without_a_main_role_the_first_regular_track_becomes_default() {
    let text = r#"
      <AdaptationSet mimeType="text/vtt" lang="en">
        <Role schemeIdUri="urn:mpeg:dash:role:2011" value="forced-subtitle"/>
        <Representation id="a"><BaseURL>a.vtt</BaseURL></Representation></AdaptationSet>
      <AdaptationSet mimeType="text/vtt" lang="ru">
        <Representation id="b"><BaseURL>b.vtt</BaseURL></Representation></AdaptationSet>"#;
    let described = describe_tracks(&build_text_plan_set(
        &mpd_with_text(text, ""),
        &base(),
        limit(),
    ));
    assert_eq!(
        described
            .iter()
            .map(|track| track.default)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
}

#[test]
fn segment_template_with_pto_uses_the_media_timeline_basis_and_player_origin_shifts_the_index() {
    let text = r#"<AdaptationSet mimeType="application/mp4" codecs="stpp.ttml.im1t" lang="en">
        <SegmentTemplate timescale="1000" duration="5000" startNumber="1"
            presentationTimeOffset="100000"
            initialization="i.mp4" media="s-$Number$.m4s"/>
        <Representation id="t"/></AdaptationSet>"#;
    let plan = build_text_plan_set(&mpd_with_text(text, ""), &base(), limit());
    let period = &plan.tracks[0].periods[0];
    assert_eq!(
        period.basis,
        DashTextTimeBasis::MediaTimeline {
            media_time_origin: Duration::from_secs(100)
        }
    );
    assert!(period.initialization.is_some());
    assert_eq!(period.segments.len(), 2, "10 s period / 5 s segments");

    // Начало шкалы плеера на 4 с: первый сегмент (0..5 с) обрезается началом, второй цел.
    let index = index_segments(plan.tracks[0].periods.iter(), 0, Duration::from_secs(4));
    let starts: Vec<(Duration, Duration)> = index
        .iter()
        .map(|segment| (segment.start, segment.duration))
        .collect();
    assert_eq!(
        starts,
        vec![
            // Покрытый отрезок 0..5 с обрезан началом шкалы (4 с): остаётся 1 с, а не 5.
            (Duration::ZERO, Duration::from_secs(1)),
            (Duration::from_secs(1), Duration::from_secs(5)),
        ]
    );
    // Ключ - начало на шкале презентации, поэтому не зависит от начала шкалы плеера.
    assert_eq!(index[1].key, 5_000_000);
    // Сегмент, закончившийся до начала шкалы плеера, не попадает в индекс.
    let late = index_segments(plan.tracks[0].periods.iter(), 0, Duration::from_secs(5));
    assert_eq!(late.len(), 1);
}

#[test]
fn single_file_wvtt_and_sidecar_ttml_plans_cover_the_whole_period() {
    let text = r#"<AdaptationSet contentType="text" lang="es">
        <Representation id="w" mimeType="application/mp4" codecs="wvtt">
          <BaseURL>es.mp4</BaseURL></Representation></AdaptationSet>
      <AdaptationSet mimeType="application/ttml+xml" lang="it">
        <Representation id="t"><BaseURL>it.ttml</BaseURL></Representation></AdaptationSet>"#;
    let plan = build_text_plan_set(&mpd_with_text(text, text), &base(), limit());
    assert_eq!(plan.tracks.len(), 2);
    let wvtt = &plan.tracks[0].periods[1];
    assert_eq!(
        wvtt.basis,
        DashTextTimeBasis::MediaTimeline {
            media_time_origin: Duration::ZERO
        }
    );
    assert_eq!(wvtt.segments.len(), 1);
    assert_eq!(wvtt.segments[0].duration, Some(Duration::from_secs(10)));
    assert!(wvtt.initialization.is_none(), "init lives inside the file");
}

#[test]
fn unplannable_text_sets_are_dropped_instead_of_failing_the_plan_set() {
    // SegmentList без duration не планируется; соседняя дорожка остаётся.
    let text = r#"<AdaptationSet mimeType="application/mp4" codecs="stpp" lang="en">
        <Representation id="x"><SegmentList timescale="1000">
          <SegmentURL media="a.m4s"/></SegmentList></Representation></AdaptationSet>
      <AdaptationSet mimeType="text/vtt" lang="ru">
        <Representation id="ok"><BaseURL>ru.vtt</BaseURL></Representation></AdaptationSet>"#;
    let plan = build_text_plan_set(&mpd_with_text(text, ""), &base(), limit());
    assert_eq!(plan.tracks.len(), 1);
    assert_eq!(plan.tracks[0].language.as_deref(), Some("ru"));
}

#[test]
fn init_cache_key_changes_with_the_initialization_resource_and_representation() {
    let key_for = |text: &str| {
        let plan = build_text_plan_set(&mpd_with_text(text, ""), &base(), limit());
        let index = index_segments(plan.tracks[0].periods.iter(), 0, Duration::ZERO);
        index[0].handle.inner.init_key
    };
    let text = |representation: &str, init: &str| {
        format!(
            r#"<AdaptationSet mimeType="application/mp4" codecs="stpp" lang="en">
            <SegmentTemplate timescale="1000" duration="5000" startNumber="1"
                initialization="{init}" media="s-$Number$.m4s"/>
            <Representation id="{representation}"/></AdaptationSet>"#
        )
    };
    let original = key_for(&text("t", "i1.mp4"));
    assert_eq!(
        original,
        key_for(&text("t", "i1.mp4")),
        "same init is reused"
    );
    assert_ne!(original, key_for(&text("t", "i2.mp4")), "new init resource");
    assert_ne!(
        original,
        key_for(&text("u", "i1.mp4")),
        "other representation"
    );
}
