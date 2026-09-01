use bounded_xml_reader::{XmlBudgetKind, XmlBudgets};
use dash_mpd_core::{
    DashAddressing, DashAudioChannelConfiguration, DashContainer, DashFrameRate, DashHdrTransfer,
    DashMediaKind, DashMpdErrorKind, DashMpdLimits, DashMpdParseRequest, DashPresentationDuration,
    DashTemplateContext, DashTemplateError, DashTimelineEntry, DashUrlReference, expand_timeline,
    parse_dash_mpd,
};

/// Test-only XML budgets; production defaults намеренно отсутствуют.
fn xml_budgets() -> XmlBudgets {
    constrained_xml_budgets(32, 32, 32 * 1024)
}

#[test]
fn standardized_representation_metadata_is_exact_inherited_and_optional() {
    let mpd = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
          xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
          xsi:schemaLocation="urn:mpeg:dash:schema:mpd:2011 DASH-MPD.xsd"
          mediaPresentationDuration="PT2S">
          <Period duration="PT2S">
            <AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f" width="1920" height="1080"
                frameRate="30000/1001">
              <SupplementalProperty schemeIdUri="urn:mpeg:mpegB:cicp:ColourPrimaries" value="9"/>
              <SupplementalProperty schemeIdUri="urn:mpeg:mpegB:cicp:TransferCharacteristics" value="16"/>
              <SupplementalProperty schemeIdUri="urn:mpeg:mpegB:cicp:MatrixCoefficients" value="9"/>
              <Representation id="hdr" bandwidth="4000000"/>
              <Representation id="override" bandwidth="2000000" width="1280" height="720"
                  frameRate="24">
                <SupplementalProperty schemeIdUri="urn:mpeg:mpegB:cicp:TransferCharacteristics" value="1"/>
              </Representation>
            </AdaptationSet>
            <AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2" lang="en-US"
                audioSamplingRate="48000">
              <AudioChannelConfiguration
                  schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011"
                  value="6"/>
              <Representation id="audio" bandwidth="128000"/>
            </AdaptationSet>
          </Period>
        </MPD>"#,
    )
    .expect("standardized metadata profile");

    let video = &mpd.periods[0].adaptation_sets[0].representations;
    assert_eq!(
        video[0].frame_rate,
        Some(DashFrameRate {
            numerator: 30_000,
            denominator: 1_001,
        })
    );
    assert_eq!(video[0].color.colour_primaries, Some(9));
    assert_eq!(
        (video[0].width, video[0].height),
        (Some(1_920), Some(1_080))
    );
    assert_eq!(video[0].color.matrix_coefficients, Some(9));
    assert_eq!(video[0].color.hdr_transfer(), Some(DashHdrTransfer::Pq));
    assert_eq!(
        video[1].frame_rate,
        Some(DashFrameRate {
            numerator: 24,
            denominator: 1,
        })
    );
    assert_eq!(video[1].color.transfer_characteristics, Some(1));
    assert_eq!(video[1].color.hdr_transfer(), None);

    let audio = &mpd.periods[0].adaptation_sets[1].representations[0];
    assert_eq!(audio.audio_sampling_rate, Some(48_000));
    assert_eq!(audio.language.as_deref(), Some("en-US"));
    assert_eq!(
        audio.audio_channel_configuration,
        Some(DashAudioChannelConfiguration::Mpeg23003_3(6))
    );
    assert_eq!(audio.frame_rate, None);
    assert_eq!(audio.color, Default::default());
}

#[test]
fn absent_metadata_stays_absent_and_invalid_or_unknown_essential_metadata_fails_closed() {
    let absent = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
          <Period duration="PT1S"><AdaptationSet mimeType="video/webm" codecs="vp9">
            <Representation id="plain"/>
          </AdaptationSet></Period>
        </MPD>"#,
    )
    .expect("metadata absence is supported");
    let representation = &absent.periods[0].adaptation_sets[0].representations[0];
    assert_eq!(representation.frame_rate, None);
    assert_eq!(representation.audio_sampling_rate, None);
    assert_eq!(representation.audio_channel_configuration, None);
    assert_eq!(representation.language, None);
    assert_eq!(representation.color, Default::default());

    let cases = [
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
          <Period duration="PT1S"><AdaptationSet mimeType="video/webm" codecs="vp9">
            <Representation id="bad" frameRate="30000/0"/>
          </AdaptationSet></Period>
        </MPD>"#,
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
          <Period duration="PT1S"><AdaptationSet mimeType="video/webm" codecs="vp9">
            <EssentialProperty schemeIdUri="urn:example:required" value="1"/>
            <Representation id="bad"/>
          </AdaptationSet></Period>
        </MPD>"#,
    ];
    assert_eq!(
        parse(cases[0]).expect_err("zero denominator").kind(),
        DashMpdErrorKind::InvalidAttribute
    );
    assert_eq!(
        parse(cases[1])
            .expect_err("unknown essential descriptor")
            .kind(),
        DashMpdErrorKind::UnsupportedConstruct
    );
}

/// Позволяет focused fixtures исчерпать один конкретный XML budget.
fn constrained_xml_budgets(
    maximum_depth: usize,
    maximum_attributes_per_element: usize,
    maximum_text_bytes: usize,
) -> XmlBudgets {
    XmlBudgets::builder()
        .maximum_document_bytes(64 * 1024)
        .maximum_depth(maximum_depth)
        .maximum_tokens(1_024)
        .maximum_attributes_per_element(maximum_attributes_per_element)
        .maximum_attribute_count(512)
        .maximum_attribute_bytes(32 * 1024)
        .maximum_namespace_declarations_per_element(8)
        .maximum_namespace_declaration_count(32)
        .maximum_namespace_bytes(4 * 1024)
        .maximum_text_bytes(maximum_text_bytes)
        .build()
        .expect("test задаёт complete XML budget")
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

/// Короткий pure parser helper.
fn parse(document: &str) -> Result<dash_mpd_core::DashMpd, dash_mpd_core::DashMpdError> {
    parse_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: limits(),
    })
}

#[test]
fn segment_template_timeline_and_base_url_inheritance_shape_are_preserved() {
    let mpd = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"
            mediaPresentationDuration="PT6S">
            <BaseURL>root/</BaseURL>
            <Period id="p0" start="PT0S" duration="PT6S">
              <BaseURL>period/</BaseURL>
              <AdaptationSet id="v" mimeType="video/mp4" contentType="video"
                  codecs="avc1.4d401f">
                <BaseURL>video/</BaseURL>
                <SegmentTemplate timescale="1000" startNumber="5"
                    initialization="init-$RepresentationID$.mp4"
                    media="chunk-$Number%05d$-$Time$.m4s">
                  <SegmentTimeline><S t="0" d="2000" r="2"/></SegmentTimeline>
                </SegmentTemplate>
                <Representation id="1080" bandwidth="4000000" width="1920" height="1080">
                  <BaseURL>main/</BaseURL>
                </Representation>
              </AdaptationSet>
            </Period>
          </MPD>"#,
    )
    .expect("поддерживаемый static template MPD");

    assert_eq!(
        mpd.media_presentation_duration,
        DashPresentationDuration::FiniteMilliseconds(6_000)
    );
    assert_eq!(
        mpd.base_url
            .as_ref()
            .expect("root base")
            .reference()
            .as_str(),
        "root/"
    );
    let period = &mpd.periods[0];
    assert_eq!(period.start_milliseconds, 0);
    assert_eq!(
        period.duration,
        DashPresentationDuration::FiniteMilliseconds(6_000)
    );
    let representation = &period.adaptation_sets[0].representations[0];
    assert_eq!(representation.container, DashContainer::IsoBmff);
    assert_eq!(representation.media_kind, DashMediaKind::Video);
    assert_eq!(representation.width, Some(1_920));
    assert_eq!(representation.height, Some(1_080));
    let DashAddressing::Template(template) = &representation.addressing else {
        panic!("ожидался inherited template");
    };
    let expanded = template
        .media
        .expand(DashTemplateContext {
            representation_id: &representation.id,
            bandwidth: representation.bandwidth,
            number: 7,
            time: 4_000,
        })
        .expect("validated template expands");
    assert_eq!(expanded, "chunk-00007-4000.m4s");
    let points = expand_timeline(&template.timeline, template.start_number, Some(6_000), 8)
        .expect("bounded timeline");
    assert_eq!(points.segments.len(), 3);
    assert_eq!(points.segments[2].start_time, 4_000);
}

#[test]
fn segment_list_and_segment_base_initialization_are_modelled() {
    let list = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT4S">
          <Period duration="PT4S"><AdaptationSet mimeType="audio/webm" codecs="opus">
            <Representation id="a">
              <SegmentList timescale="1000" duration="2000">
                <Initialization sourceURL="init.webm"/>
                <SegmentURL media="one.webm" mediaRange="0-99"/>
                <SegmentURL media="two.webm"/>
              </SegmentList>
            </Representation>
          </AdaptationSet></Period>
        </MPD>"#,
    )
    .expect("SegmentList");
    let representation = &list.periods[0].adaptation_sets[0].representations[0];
    assert_eq!(representation.container, DashContainer::WebM);
    assert_eq!(representation.media_kind, DashMediaKind::Audio);
    let DashAddressing::List(segment_list) = &representation.addressing else {
        panic!("ожидался SegmentList");
    };
    assert_eq!(segment_list.segments.len(), 2);
    assert_eq!(
        segment_list.segments[0].media_range.expect("range").end(),
        99
    );

    let base = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT2S">
          <Period duration="PT2S"><AdaptationSet mimeType="video/mp4" codecs="av01.0.05M.08">
            <Representation id="v"><BaseURL>single.mp4</BaseURL>
              <SegmentBase timescale="1000" indexRange="100-199">
                <Initialization range="0-99"/>
              </SegmentBase>
            </Representation>
          </AdaptationSet></Period>
        </MPD>"#,
    )
    .expect("SegmentBase");
    let DashAddressing::Base(segment_base) =
        &base.periods[0].adaptation_sets[0].representations[0].addressing
    else {
        panic!("ожидался SegmentBase");
    };
    assert_eq!(segment_base.index_range.expect("index").start(), 100);
    assert_eq!(
        segment_base
            .initialization
            .as_ref()
            .and_then(|initialization| initialization.byte_range)
            .expect("init range")
            .end(),
        99
    );
}

#[test]
fn known_non_playback_text_adaptation_does_not_hide_or_mutate_av_catalog() {
    let parsed = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT2S">
          <Period duration="PT2S">
            <AdaptationSet contentType="text" lang="en" subsegmentAlignment="true">
              <Representation id="captions" mimeType="application/mp4" codecs="wvtt">
                <BaseURL>captions.mp4</BaseURL>
                <SegmentBase indexRange="0-31"><Initialization range="0-15"/></SegmentBase>
              </Representation>
            </AdaptationSet>
            <AdaptationSet contentType="text" mimeType="text/vtt" lang="fr">
              <Representation id="captions-vtt"><BaseURL>captions.vtt</BaseURL></Representation>
            </AdaptationSet>
            <AdaptationSet contentType="video" mimeType="video/mp4" codecs="avc1.4d401f">
              <Representation id="unsupported-sar" sar="2:1"/>
              <Representation id="video"><BaseURL>video.mp4</BaseURL>
                <SegmentBase indexRange="100-199"><Initialization range="0-99"/></SegmentBase>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>"#,
    )
    .expect("known subtitle row рядом с playable A/V catalog");

    assert_eq!(parsed.periods[0].adaptation_sets.len(), 1);
    let retained = &parsed.periods[0].adaptation_sets[0].representations[0];
    assert_eq!(retained.id, "video");
    assert_eq!(retained.media_kind, DashMediaKind::Video);
}

#[test]
fn exact_namespace_dynamic_drm_base_cardinality_and_media_mismatch_fail_closed() {
    let cases = [
        (
            r#"<MPD xmlns="urn:not-dash" mediaPresentationDuration="PT1S"/>"#,
            DashMpdErrorKind::InvalidRoot,
        ),
        (
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
                mediaPresentationDuration="PT1S"></MPD>"#,
            DashMpdErrorKind::DynamicPresentation,
        ),
        (
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" xmlns:vendor="urn:vendor"
                vendor:opaque="true" mediaPresentationDuration="PT1S">
               <Period duration="PT1S"><AdaptationSet mimeType="audio/webm" codecs="opus">
                 <Representation id="vendor-attribute"/></AdaptationSet></Period></MPD>"#,
            DashMpdErrorKind::UnsupportedConstruct,
        ),
        (
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
               <Period duration="PT1S"><AdaptationSet mimeType="audio/webm" codecs="opus"
                 subsegmentAlignment="sometimes">
                 <Representation id="bad-alignment"/></AdaptationSet></Period></MPD>"#,
            DashMpdErrorKind::InvalidAttribute,
        ),
        (
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
               <Period duration="PT1S"><AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
                 <ContentProtection/><Representation id="v"/>
               </AdaptationSet></Period></MPD>"#,
            DashMpdErrorKind::ContentProtection,
        ),
        (
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
               <BaseURL>a/</BaseURL><BaseURL>b/</BaseURL>
               <Period duration="PT1S"><AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
                 <Representation id="v"/>
               </AdaptationSet></Period></MPD>"#,
            DashMpdErrorKind::MultipleBaseUrls,
        ),
        (
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
               <Period duration="PT1S"><AdaptationSet mimeType="audio/mp4" contentType="audio"
                 codecs="avc1.4d401f"><Representation id="broken"/></AdaptationSet></Period></MPD>"#,
            DashMpdErrorKind::UnsupportedMediaEvidence,
        ),
        (
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
               <Period duration="PT1S"><AdaptationSet mimeType="video/webm" codecs="avc1.4d401f">
                 <Representation id="container-mismatch"/></AdaptationSet></Period></MPD>"#,
            DashMpdErrorKind::UnsupportedMediaEvidence,
        ),
        (
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
               <Period duration="PT1S"><AdaptationSet contentType="text"
                 mimeType="application/mp4" codecs="unknown-text">
                 <Representation id="unknown-text"/></AdaptationSet></Period></MPD>"#,
            DashMpdErrorKind::UnsupportedMediaEvidence,
        ),
        (
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
               <Period duration="PT1S"><AdaptationSet contentType="text"
                 mimeType="application/mp4" codecs="wvtt">
                 <ContentProtection/><Representation id="protected-text"/>
               </AdaptationSet></Period></MPD>"#,
            DashMpdErrorKind::ContentProtection,
        ),
        (
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S2M">
               <Period duration="PT1S"><AdaptationSet mimeType="audio/webm" codecs="opus">
                 <Representation id="bad-duration"/></AdaptationSet></Period></MPD>"#,
            DashMpdErrorKind::InvalidAttribute,
        ),
    ];
    for (document, expected) in cases {
        assert_eq!(
            parse(document).expect_err("должен быть reject").kind(),
            expected
        );
    }
}

#[test]
fn declared_profiles_are_exactly_allowlisted_and_do_not_expand_the_syntax_profile() {
    let supported_profiles = [
        "urn:mpeg:dash:profile:full:2011",
        "urn:mpeg:dash:profile:isoff-on-demand:2011",
        "urn:mpeg:dash:profile:isoff-live:2011",
        "urn:mpeg:dash:profile:isoff-main:2011",
        "urn:mpeg:dash:profile:webm-on-demand:2012",
    ]
    .join(", ");
    let supported_document = format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
             profiles="{supported_profiles}" mediaPresentationDuration="PT1S">
             <Period duration="PT1S"><AdaptationSet mimeType="audio/webm" codecs="opus">
               <Representation id="a"/>
             </AdaptationSet></Period>
           </MPD>"#
    );
    parse(&supported_document).expect("каждый доказанный profile разрешён");

    let rejected_profiles = [
        "",
        "urn:mpeg:dash:profile:unknown:2011",
        "urn:mpeg:dash:profile:mp2t-main:2011",
        "urn:3GPP:ns:PSS:AdaptiveHTTPStreaming:MPD:2011",
        "urn:dvb:dash:profile:dvb-dash:2014",
        "urn:hbbtv:dash:profile:isoff-live:2012",
        "urn:mpeg:dash:profile:isoff-main:2011,",
        "urn:mpeg:dash:profile:isoff-main:2011,,urn:mpeg:dash:profile:full:2011",
    ];
    for rejected_profile in rejected_profiles {
        let document = format!(
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
                 profiles="{rejected_profile}" mediaPresentationDuration="PT1S">
                 <Period duration="PT1S"><AdaptationSet mimeType="audio/webm" codecs="opus">
                   <Representation id="a"/>
                 </AdaptationSet></Period>
               </MPD>"#
        );
        assert_eq!(
            parse(&document)
                .expect_err("неизвестный или пустой profile запрещён")
                .kind(),
            DashMpdErrorKind::UnsupportedProfile
        );
    }
}

#[test]
fn multi_period_must_be_exactly_contiguous_and_finite() {
    let mpd = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT4S">
          <Period start="PT0S" duration="PT2S"><AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2">
            <Representation id="a0"/>
          </AdaptationSet></Period>
          <Period start="PT2S" duration="PT2S"><AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2">
            <Representation id="a1"/>
          </AdaptationSet></Period>
        </MPD>"#,
    )
    .expect("aligned finite periods");
    assert_eq!(mpd.periods.len(), 2);

    let error = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT5S">
          <Period duration="PT2S"><AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2">
            <Representation id="a0"/></AdaptationSet></Period>
          <Period start="PT3S" duration="PT2S"><AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2">
            <Representation id="a1"/></AdaptationSet></Period>
        </MPD>"#,
    )
    .expect_err("gap запрещён");
    assert_eq!(error.kind(), DashMpdErrorKind::InvalidPeriodTimeline);
}

#[test]
fn timeline_repeat_is_bounded_and_checked_for_overflow() {
    let entries = [DashTimelineEntry {
        start_time: Some(0),
        duration: 2,
        repeat: -1,
    }];
    assert_eq!(
        expand_timeline(&entries, 1, None, 16),
        Err(DashTemplateError::UnboundedRepeat)
    );
    assert_eq!(
        expand_timeline(&entries, 1, Some(100), 4),
        Err(DashTemplateError::ExpansionLimit)
    );
    assert_eq!(
        expand_timeline(&entries, 1, Some(99), 64),
        Err(DashTemplateError::UnboundedRepeat)
    );
    let overflow = [DashTimelineEntry {
        start_time: Some(u64::MAX - 1),
        duration: 2,
        repeat: 1,
    }];
    assert_eq!(
        expand_timeline(&overflow, 1, None, 4),
        Err(DashTemplateError::ArithmeticOverflow)
    );
}

#[test]
fn hardened_xml_budget_and_doctype_rejections_are_preserved() {
    let request = DashMpdParseRequest {
        document_bytes: br#"<!DOCTYPE MPD><MPD xmlns="urn:mpeg:dash:schema:mpd:2011"/>"#,
        xml_budgets: xml_budgets(),
        limits: limits(),
    };
    assert_eq!(
        parse_dash_mpd(request)
            .expect_err("DOCTYPE запрещён")
            .kind(),
        DashMpdErrorKind::Xml
    );

    let tiny_budget = XmlBudgets::builder()
        .maximum_document_bytes(8)
        .maximum_depth(1)
        .maximum_tokens(1)
        .maximum_attributes_per_element(1)
        .maximum_attribute_count(1)
        .maximum_attribute_bytes(1)
        .maximum_namespace_declarations_per_element(1)
        .maximum_namespace_declaration_count(1)
        .maximum_namespace_bytes(1)
        .maximum_text_bytes(1)
        .build()
        .expect("complete tiny budget");
    let error = parse_dash_mpd(DashMpdParseRequest {
        document_bytes: br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"/>"#,
        xml_budgets: tiny_budget,
        limits: limits(),
    })
    .expect_err("byte cap");
    assert_eq!(error.kind(), DashMpdErrorKind::Xml);

    // Импорт подтверждает, что XML budget vocabulary остаётся external boundary.
    let _document_budget = XmlBudgetKind::DocumentBytes;
    let _reference_type = std::any::TypeId::of::<DashUrlReference>();
}

#[test]
fn entity_depth_attribute_text_and_truncation_fail_at_hardened_xml_boundary() {
    let fixtures = [
        (
            br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
                  <BaseURL>&custom;</BaseURL></MPD>"#
                .as_slice(),
            xml_budgets(),
        ),
        (
            br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
                  <Period duration="PT1S"><AdaptationSet mimeType="audio/webm" codecs="opus">
                    <Representation id="a"/></AdaptationSet></Period></MPD>"#
                .as_slice(),
            constrained_xml_budgets(2, 32, 32 * 1024),
        ),
        (
            br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"
                  mediaPresentationDuration="PT1S"></MPD>"#
                .as_slice(),
            constrained_xml_budgets(32, 1, 32 * 1024),
        ),
        (
            br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
                  <BaseURL>long-text</BaseURL></MPD>"#
                .as_slice(),
            constrained_xml_budgets(32, 32, 2),
        ),
        (
            br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"><Period></MP"#.as_slice(),
            xml_budgets(),
        ),
    ];
    for (document_bytes, budgets) in fixtures {
        let error = parse_dash_mpd(DashMpdParseRequest {
            document_bytes,
            xml_budgets: budgets,
            limits: limits(),
        })
        .expect_err("hardened XML fixture должен быть отвергнут");
        assert_eq!(error.kind(), DashMpdErrorKind::Xml);
    }
}
