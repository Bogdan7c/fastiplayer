// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Описательные дети корня MPD (`ProgramInformation`, `ServiceDescription`) и булев
//! hint `bitstreamSwitching`: оба пишет `ffmpeg -f dash`, на воспроизведение они не влияют.

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{
    DashAddressing, DashMpd, DashMpdError, DashMpdErrorKind, DashMpdLimits, DashMpdParseRequest,
    DashPresentationDuration, parse_dash_mpd,
};

/// Точный MPD, который выдаёт `ffmpeg -f dash` (включая табы, `$Number%05d$`, timescale 1e6).
const FFMPEG_DASH_MPD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
	xmlns="urn:mpeg:dash:schema:mpd:2011"
	xmlns:xlink="http://www.w3.org/1999/xlink"
	xsi:schemaLocation="urn:mpeg:DASH:schema:MPD:2011 http://standards.iso.org/ittf/PubliclyAvailableStandards/MPEG-DASH_schema_files/DASH-MPD.xsd"
	profiles="urn:mpeg:dash:profile:isoff-live:2011"
	type="static"
	mediaPresentationDuration="PT1M30.0S"
	maxSegmentDuration="PT6.0S"
	minBufferTime="PT12.0S">
	<ProgramInformation>
	</ProgramInformation>
	<ServiceDescription id="0">
	</ServiceDescription>
	<Period id="0" start="PT0.0S">
		<AdaptationSet id="0" contentType="video" startWithSAP="1" segmentAlignment="true" bitstreamSwitching="true" frameRate="24/1" maxWidth="1280" maxHeight="544" par="40:17" lang="eng">
			<Representation id="0" mimeType="video/mp4" codecs="avc1.64001f" bandwidth="1450630" width="1280" height="544" sar="1:1">
				<SegmentTemplate timescale="1000000" duration="6000000" initialization="init-$RepresentationID$.m4s" media="chunk-$RepresentationID$-$Number%05d$.m4s" startNumber="1">
				</SegmentTemplate>
			</Representation>
		</AdaptationSet>
		<AdaptationSet id="1" contentType="audio" startWithSAP="1" segmentAlignment="true" bitstreamSwitching="true" lang="eng">
			<Representation id="1" mimeType="audio/mp4" codecs="mp4a.40.2" bandwidth="132651" audioSamplingRate="48000">
				<AudioChannelConfiguration schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011" value="2" />
				<SegmentTemplate timescale="1000000" duration="6000000" initialization="init-$RepresentationID$.m4s" media="chunk-$RepresentationID$-$Number%05d$.m4s" startNumber="1">
				</SegmentTemplate>
			</Representation>
		</AdaptationSet>
	</Period>
</MPD>
"#;

/// Test-only XML budgets с настраиваемой глубиной.
fn xml_budgets(maximum_depth: usize) -> XmlBudgets {
    XmlBudgets::builder()
        .maximum_document_bytes(64 * 1024)
        .maximum_depth(maximum_depth)
        .maximum_tokens(4_096)
        .maximum_attributes_per_element(32)
        .maximum_attribute_count(512)
        .maximum_attribute_bytes(32 * 1024)
        .maximum_namespace_declarations_per_element(8)
        .maximum_namespace_declaration_count(32)
        .maximum_namespace_bytes(4 * 1024)
        .maximum_text_bytes(32 * 1024)
        .build()
        .expect("XML test budgets are valid")
}

/// Test-only schema caps.
fn limits() -> DashMpdLimits {
    DashMpdLimits {
        maximum_periods: 8,
        maximum_adaptation_sets_per_period: 8,
        maximum_representations_per_adaptation_set: 16,
        maximum_segments_per_list: 64,
        maximum_timeline_entries: 64,
        maximum_schema_string_bytes: 4 * 1024,
    }
}

/// Разбирает документ production entry point-ом с заданной глубиной XML.
fn parse_with_depth(document: &str, maximum_depth: usize) -> Result<DashMpd, DashMpdError> {
    parse_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: xml_budgets(maximum_depth),
        limits: limits(),
    })
}

/// Разбирает документ с запасом по глубине.
fn parse(document: &str) -> Result<DashMpd, DashMpdError> {
    parse_with_depth(document, 32)
}

/// Минимальный MPD с подстановкой фрагментов в корень и в AdaptationSet.
fn minimal_mpd(root_children: &str, adaptation_attributes: &str) -> String {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"
            mediaPresentationDuration="PT2S">
          {root_children}
          <Period duration="PT2S">
            <AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f" {adaptation_attributes}>
              <Representation id="video" bandwidth="1000" width="640" height="360">
                <SegmentTemplate timescale="1" duration="1" media="v-$Number$.m4s"
                    initialization="v-init.mp4"/>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>"#
    )
}

#[test]
fn ffmpeg_dash_mpd_parses_into_two_adaptation_sets_with_templates_and_exact_duration() {
    let mpd = parse(FFMPEG_DASH_MPD).expect("ffmpeg MPD is within the supported profile");

    assert_eq!(
        mpd.media_presentation_duration,
        DashPresentationDuration::FiniteMilliseconds(90_000)
    );
    assert_eq!(mpd.periods.len(), 1);
    let adaptation_sets = &mpd.periods[0].adaptation_sets;
    assert_eq!(adaptation_sets.len(), 2);

    let video = &adaptation_sets[0].representations[0];
    assert_eq!((video.width, video.height), (Some(1_280), Some(544)));
    let DashAddressing::Template(video_template) = &video.addressing else {
        panic!("video must keep SegmentTemplate addressing");
    };
    assert_eq!(video_template.timescale, 1_000_000);
    assert_eq!(video_template.duration, Some(6_000_000));
    assert_eq!(
        video_template.media.as_str(),
        "chunk-$RepresentationID$-$Number%05d$.m4s"
    );
    assert_eq!(
        video_template
            .initialization
            .as_ref()
            .map(|init| init.as_str()),
        Some("init-$RepresentationID$.m4s")
    );

    let audio = &adaptation_sets[1].representations[0];
    assert_eq!(audio.audio_sampling_rate, Some(48_000));
    assert!(matches!(audio.addressing, DashAddressing::Template(_)));
}

#[test]
fn informational_root_elements_with_children_are_skipped_without_changing_the_result() {
    let plain = parse(&minimal_mpd("", "")).expect("plain MPD");
    let with_information = parse(&minimal_mpd(
        r#"<ProgramInformation moreInformationURL="https://example.invalid/info" lang="en">
             <Title>Some title</Title>
             <Source>Some source</Source>
             <Copyright>Some copyright</Copyright>
           </ProgramInformation>
           <ServiceDescription id="7">
             <Latency target="3500" min="2000" max="6000"/>
             <PlaybackRate min="0.96" max="1.04"></PlaybackRate>
             <Scope schemeIdUri="urn:example:scope"/>
           </ServiceDescription>
           <ProgramInformation/>
           <ServiceDescription id="8"/>"#,
        "",
    ))
    .expect("informational root children are ignorable");
    assert_eq!(plain, with_information);
}

#[test]
fn bitstream_switching_is_a_validated_boolean_hint_that_does_not_change_the_result() {
    let plain = parse(&minimal_mpd("", "")).expect("plain MPD");
    for accepted in ["true", "false", "1", "0"] {
        let hinted = parse(&minimal_mpd(
            "",
            &format!(r#"bitstreamSwitching="{accepted}""#),
        ))
        .expect("boolean hint is accepted");
        assert_eq!(
            plain, hinted,
            "value {accepted} must not leak into the model"
        );
    }
    let error = parse(&minimal_mpd("", r#"bitstreamSwitching="maybe""#))
        .expect_err("non-boolean hint is a typed attribute error");
    assert_eq!(error.kind(), DashMpdErrorKind::InvalidAttribute);
}

#[test]
fn bitstream_switching_is_accepted_on_representation_and_segment_template() {
    let document = minimal_mpd("", "")
        .replace(
            r#"<Representation id="video""#,
            r#"<Representation bitstreamSwitching="true" id="video""#,
        )
        .replace(
            "<SegmentTemplate ",
            r#"<SegmentTemplate bitstreamSwitching="false" "#,
        );
    let hinted = parse(&document).expect("hint on Representation and SegmentTemplate");
    assert_eq!(parse(&minimal_mpd("", "")).expect("plain MPD"), hinted);

    let invalid = document.replace(r#"bitstreamSwitching="false""#, r#"bitstreamSwitching="x""#);
    assert_eq!(
        parse(&invalid).expect_err("invalid template hint").kind(),
        DashMpdErrorKind::InvalidAttribute
    );
}

#[test]
fn unknown_root_children_and_attributes_stay_fail_closed() {
    for root_child in [
        "<Metrics/>",
        "<ProgramInformations/>",
        "<Unknown><Title/></Unknown>",
    ] {
        let error = parse(&minimal_mpd(root_child, "")).expect_err("unknown root child");
        assert_eq!(error.kind(), DashMpdErrorKind::UnsupportedConstruct);
    }
    let error = parse(&minimal_mpd("", r#"mysteryHint="1""#)).expect_err("unknown attribute");
    assert_eq!(error.kind(), DashMpdErrorKind::UnsupportedConstruct);
}

#[test]
fn content_protection_next_to_or_inside_informational_elements_is_still_rejected() {
    let cases = [
        r#"<ProgramInformation/><ContentProtection schemeIdUri="urn:example:drm"/>"#,
        r#"<ServiceDescription/><ContentProtection schemeIdUri="urn:example:drm"></ContentProtection>"#,
        r#"<ProgramInformation><ContentProtection schemeIdUri="urn:example:drm"/></ProgramInformation>"#,
        r#"<ServiceDescription><Latency><ContentProtection schemeIdUri="urn:example:drm"></ContentProtection></Latency></ServiceDescription>"#,
    ];
    for root_children in cases {
        let error = parse(&minimal_mpd(root_children, "")).expect_err("DRM must not hide");
        assert_eq!(error.kind(), DashMpdErrorKind::ContentProtection);
    }
}

#[test]
fn deeply_nested_informational_content_still_hits_the_xml_depth_budget() {
    let nested = format!(
        "<ProgramInformation>{}{}</ProgramInformation>",
        "<Title>".repeat(40),
        "</Title>".repeat(40)
    );
    let document = minimal_mpd(&nested, "");
    let error = parse_with_depth(&document, 16).expect_err("depth budget applies to skipped data");
    assert_eq!(error.kind(), DashMpdErrorKind::Xml);
    // Тот же документ с достаточным бюджетом глубины проходит: отказ вызван именно бюджетом.
    parse_with_depth(&document, 64).expect("generous depth budget accepts the skipped subtree");
}
