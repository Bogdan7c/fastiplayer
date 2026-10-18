// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Текстовые AdaptationSet: разбор в отдельную модель (четыре формата, роли, название,
//! адресация), пропуск DRM/неизвестного без вреда для A/V и неизменность каталога A/V.

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{
    DashAddressing, DashMediaKind, DashMpd, DashMpdErrorKind, DashMpdLimits, DashMpdParseRequest,
    DashPresentationDuration, DashTextKind, DashTextRole, parse_dash_mpd, parse_dynamic_dash_mpd,
};

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
        .expect("XML test budgets are valid")
}

fn limits() -> DashMpdLimits {
    DashMpdLimits {
        maximum_periods: 8,
        maximum_adaptation_sets_per_period: 16,
        maximum_representations_per_adaptation_set: 8,
        maximum_segments_per_list: 32,
        maximum_timeline_entries: 64,
        maximum_schema_string_bytes: 4 * 1024,
    }
}

/// Один видео-набор, рядом с которым стоят проверяемые текстовые наборы.
fn static_mpd(text_sets: &str) -> String {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT10S">
          <Period duration="PT10S">
            <AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
              <Representation id="video"><BaseURL>video.mp4</BaseURL></Representation>
            </AdaptationSet>
            {text_sets}
          </Period>
        </MPD>"#
    )
}

fn parse(document: &str) -> DashMpd {
    parse_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: limits(),
    })
    .expect("MPD must parse")
}

#[test]
fn all_four_text_kinds_are_parsed_with_language_label_and_addressing() {
    let mpd = parse(&static_mpd(
        r#"<AdaptationSet contentType="text" mimeType="application/ttml+xml" lang="en">
             <Role schemeIdUri="urn:mpeg:dash:role:2011" value="subtitle"/>
             <Label>English</Label>
             <Representation id="ttml"><BaseURL>en.ttml</BaseURL></Representation>
           </AdaptationSet>
           <AdaptationSet mimeType="text/vtt" lang="fr">
             <Representation id="vtt"><BaseURL>fr.vtt</BaseURL></Representation>
           </AdaptationSet>
           <AdaptationSet mimeType="application/mp4" codecs="stpp.ttml.im1t" lang="de">
             <SegmentTemplate timescale="1000" duration="2000" startNumber="1"
                 presentationTimeOffset="500"
                 initialization="de-init.mp4" media="de-$Number$.m4s"/>
             <Representation id="stpp" bandwidth="1000"/>
           </AdaptationSet>
           <AdaptationSet contentType="text" lang="es">
             <Representation id="wvtt" mimeType="application/mp4" codecs="wvtt">
               <BaseURL>es.mp4</BaseURL>
               <SegmentBase indexRange="0-31"><Initialization range="0-15"/></SegmentBase>
             </Representation>
           </AdaptationSet>"#,
    ));
    let period = &mpd.periods[0];
    assert_eq!(period.skipped_text_adaptation_set_count, 0);
    let sets = &period.text_adaptation_sets;
    assert_eq!(sets.len(), 4);

    let ttml = &sets[0].representations[0];
    assert_eq!(ttml.kind, DashTextKind::SidecarTtml);
    assert_eq!(ttml.language.as_deref(), Some("en"));
    assert_eq!(ttml.label.as_deref(), Some("English"));
    assert_eq!(&*sets[0].roles, &[DashTextRole::Subtitle]);
    assert!(matches!(ttml.addressing, DashAddressing::SingleResource));
    assert_eq!(
        ttml.base_url.as_ref().unwrap().reference().as_str(),
        "en.ttml"
    );

    let vtt = &sets[1].representations[0];
    assert_eq!(vtt.kind, DashTextKind::SidecarWebVtt);
    assert_eq!(vtt.language.as_deref(), Some("fr"));

    // Адресация SegmentTemplate наследуется Representation от набора, PTO сохранён.
    let stpp = &sets[2].representations[0];
    assert_eq!(stpp.kind, DashTextKind::SegmentedStpp);
    let DashAddressing::Template(template) = &stpp.addressing else {
        panic!("stpp must inherit the SegmentTemplate");
    };
    assert_eq!(template.presentation_time_offset, 500);
    assert_eq!(template.duration, Some(2000));
    assert_eq!(stpp.bandwidth, Some(1000));

    let wvtt = &sets[3].representations[0];
    assert_eq!(wvtt.kind, DashTextKind::SegmentedWvtt);
    assert_eq!(wvtt.language.as_deref(), Some("es"));
    assert!(matches!(wvtt.addressing, DashAddressing::Base(_)));
}

#[test]
fn segment_timeline_and_segment_list_addressing_are_kept_for_text() {
    let mpd = parse(&static_mpd(
        r#"<AdaptationSet mimeType="application/mp4" codecs="wvtt" lang="en">
             <Representation id="timeline">
               <SegmentTemplate timescale="1000" media="t-$Time$.m4s" initialization="t-init.mp4">
                 <SegmentTimeline><S t="0" d="5000" r="1"/></SegmentTimeline>
               </SegmentTemplate>
             </Representation>
           </AdaptationSet>
           <AdaptationSet mimeType="application/mp4" codecs="stpp" lang="ru">
             <Representation id="list">
               <SegmentList timescale="1000" duration="5000">
                 <Initialization sourceURL="i.mp4"/>
                 <SegmentURL media="a.m4s"/><SegmentURL media="b.m4s"/>
               </SegmentList>
             </Representation>
           </AdaptationSet>"#,
    ));
    let sets = &mpd.periods[0].text_adaptation_sets;
    let DashAddressing::Template(template) = &sets[0].representations[0].addressing else {
        panic!("timeline expected");
    };
    assert_eq!(template.timeline.len(), 1);
    assert_eq!(template.timeline[0].repeat, 1);
    let DashAddressing::List(list) = &sets[1].representations[0].addressing else {
        panic!("list expected");
    };
    assert_eq!(list.segments.len(), 2);
}

#[test]
fn roles_are_typed_and_forced_subtitle_is_distinguished() {
    let mpd = parse(&static_mpd(
        r#"<AdaptationSet mimeType="text/vtt" lang="en">
             <Role schemeIdUri="urn:mpeg:dash:role:2011" value="forced-subtitle"/>
             <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
             <Accessibility schemeIdUri="urn:mpeg:dash:role:2011" value="caption"/>
             <Accessibility schemeIdUri="urn:tva:metadata:cs:AudioPurposeCS:2007" value="2"/>
             <Role schemeIdUri="urn:vendor:unknown" value="x"/>
             <Representation id="r"><BaseURL>a.vtt</BaseURL></Representation>
           </AdaptationSet>
           <AdaptationSet mimeType="text/vtt" lang="en">
             <Role schemeIdUri="urn:mpeg:dash:role:2011" value="something-new"/>
             <Representation id="r2"><BaseURL>b.vtt</BaseURL></Representation>
           </AdaptationSet>"#,
    ));
    let sets = &mpd.periods[0].text_adaptation_sets;
    assert_eq!(
        &*sets[0].roles,
        &[
            DashTextRole::ForcedSubtitle,
            DashTextRole::Main,
            DashTextRole::Caption
        ],
        "unknown scheme ignored, known ones kept in manifest order"
    );
    assert_eq!(&*sets[1].roles, &[DashTextRole::Other]);
}

#[test]
fn representation_label_and_language_override_the_set() {
    let mpd = parse(&static_mpd(
        r#"<AdaptationSet mimeType="text/vtt" lang="en"><Label>Set</Label>
             <Representation id="a" lang="pt"><Label>Rep</Label>
               <BaseURL>a.vtt</BaseURL></Representation>
             <Representation id="b"><BaseURL>b.vtt</BaseURL></Representation>
           </AdaptationSet>"#,
    ));
    let reps = &mpd.periods[0].text_adaptation_sets[0].representations;
    assert_eq!(reps[0].language.as_deref(), Some("pt"));
    assert_eq!(reps[0].label.as_deref(), Some("Rep"));
    assert_eq!(reps[1].language.as_deref(), Some("en"));
    assert_eq!(reps[1].label.as_deref(), Some("Set"));
}

#[test]
fn unsupported_and_protected_text_sets_are_skipped_without_touching_av() {
    let without_text = parse(&static_mpd(""));
    let with_broken_text = parse(&static_mpd(
        r#"<AdaptationSet contentType="text" mimeType="application/mp4" codecs="unknown-text">
             <Representation id="unknown"/></AdaptationSet>
           <AdaptationSet contentType="text">
             <Representation id="drift" mimeType="application/octet-stream"/></AdaptationSet>
           <AdaptationSet mimeType="application/mp4" codecs="wvtt">
             <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
             <Representation id="drm"/></AdaptationSet>
           <AdaptationSet mimeType="application/mp4" codecs="wvtt">
             <Representation id="drm-rep">
               <ContentProtection schemeIdUri="urn:vendor"/></Representation></AdaptationSet>
           <AdaptationSet mimeType="text/vtt">
             <SegmentTemplate media="missing-duration-$Number$.vtt"/>
             <Representation id="broken-template"/></AdaptationSet>
           <AdaptationSet mimeType="text/vtt" lang="en">
             <Representation id="good"><BaseURL>ok.vtt</BaseURL></Representation></AdaptationSet>"#,
    ));
    let period = &with_broken_text.periods[0];
    assert_eq!(period.skipped_text_adaptation_set_count, 5);
    assert_eq!(period.text_adaptation_sets.len(), 1);
    assert_eq!(period.text_adaptation_sets[0].representations[0].id, "good");
    // Каталог A/V байт-в-байт тот же, что без текстовых наборов.
    assert_eq!(
        period.adaptation_sets,
        without_text.periods[0].adaptation_sets
    );
    assert_eq!(
        period.adaptation_sets[0].representations[0].media_kind,
        DashMediaKind::Video
    );
}

#[test]
fn mixed_stpp_and_wvtt_in_one_representation_is_unsupported() {
    let mpd = parse(&static_mpd(
        r#"<AdaptationSet mimeType="application/mp4" codecs="stpp,wvtt">
             <Representation id="mixed"/></AdaptationSet>"#,
    ));
    assert!(mpd.periods[0].text_adaptation_sets.is_empty());
    assert_eq!(mpd.periods[0].skipped_text_adaptation_set_count, 1);
}

#[test]
fn period_with_only_text_sets_keeps_the_av_error_semantics() {
    let document = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
        <Period duration="PT1S"><AdaptationSet mimeType="text/vtt">
          <Representation id="t"><BaseURL>a.vtt</BaseURL></Representation>
        </AdaptationSet></Period></MPD>"#;
    let error = parse_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: limits(),
    })
    .expect_err("a Period without A/V is still malformed");
    assert_eq!(error.kind(), DashMpdErrorKind::MalformedSchema);
}

#[test]
fn xml_level_damage_inside_a_text_set_is_still_fatal() {
    let document = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
        <Period duration="PT1S"><AdaptationSet mimeType="text/vtt"><Representation id="t">
        </AdaptationSet></Period></MPD>"#;
    assert!(
        parse_dash_mpd(DashMpdParseRequest {
            document_bytes: document.as_bytes(),
            xml_budgets: xml_budgets(),
            limits: limits(),
        })
        .is_err()
    );
}

#[test]
fn dynamic_mpd_exposes_text_sets_with_segment_timeline() {
    let document = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
            availabilityStartTime="2026-07-24T10:00:00Z" publishTime="2026-07-24T10:01:00Z"
            minimumUpdatePeriod="PT2S" suggestedPresentationDelay="PT6S">
          <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014" value="2026-07-24T10:01:01Z"/>
          <Period id="p0" start="PT0S">
            <AdaptationSet id="v" mimeType="video/mp4" codecs="avc1.4d401f">
              <SegmentTemplate timescale="1000" media="v-$Time$.m4s" initialization="v-init.mp4">
                <SegmentTimeline><S t="0" d="2000" r="9"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="video"/>
            </AdaptationSet>
            <AdaptationSet mimeType="application/mp4" codecs="stpp" lang="en">
              <SegmentTemplate timescale="1000" media="t-$Time$.m4s" initialization="t-init.mp4">
                <SegmentTimeline><S t="0" d="2000" r="9"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="text"/>
            </AdaptationSet>
          </Period>
        </MPD>"#;
    let dynamic = parse_dynamic_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: limits(),
    })
    .expect("dynamic MPD with a text set parses");
    let period = &dynamic.presentation.periods[0];
    assert!(matches!(
        period.duration,
        DashPresentationDuration::OpenEnded
    ));
    assert_eq!(period.adaptation_sets.len(), 1);
    assert_eq!(period.text_adaptation_sets.len(), 1);
    assert_eq!(
        period.text_adaptation_sets[0].representations[0].kind,
        DashTextKind::SegmentedStpp
    );
}

#[test]
fn text_evidence_only_on_representations_is_detected_for_stpp_and_wvtt() {
    let mpd = parse(&static_mpd(
        r#"<AdaptationSet lang="en">
             <Representation id="s" mimeType="application/mp4" codecs="stpp.ttml.im1t">
               <BaseURL>s.mp4</BaseURL></Representation></AdaptationSet>
           <AdaptationSet lang="fr">
             <Representation id="w" mimeType="application/mp4" codecs="wvtt">
               <BaseURL>w.mp4</BaseURL></Representation></AdaptationSet>
           <AdaptationSet lang="de">
             <Representation id="v" mimeType="text/vtt"><BaseURL>d.vtt</BaseURL></Representation>
           </AdaptationSet>"#,
    ));
    let period = &mpd.periods[0];
    let kinds: Vec<_> = period
        .text_adaptation_sets
        .iter()
        .map(|set| set.representations[0].kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            DashTextKind::SegmentedStpp,
            DashTextKind::SegmentedWvtt,
            DashTextKind::SidecarWebVtt
        ]
    );
    // Каталог A/V по-прежнему состоит из одного видео-набора.
    assert_eq!(period.adaptation_sets.len(), 1);
}

#[test]
fn a_set_mixing_text_and_av_representations_keeps_the_av_error_semantics() {
    let document = static_mpd(
        r#"<AdaptationSet>
             <Representation id="t" mimeType="application/mp4" codecs="wvtt"/>
             <Representation id="v" mimeType="video/mp4" codecs="avc1.4d401f"/>
           </AdaptationSet>"#,
    );
    // Через A/V-разбор текстовая Representation отвергается локально, видео остаётся.
    let mpd = parse(&document);
    assert!(mpd.periods[0].text_adaptation_sets.is_empty());
    assert_eq!(mpd.periods[0].adaptation_sets.len(), 2);
    // Набор без признаков вообще остаётся ошибкой A/V.
    let broken = static_mpd(r#"<AdaptationSet><Representation id="x"/></AdaptationSet>"#);
    let error = parse_dash_mpd(DashMpdParseRequest {
        document_bytes: broken.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: limits(),
    })
    .expect_err("unproven media stays an error");
    assert_eq!(error.kind(), DashMpdErrorKind::UnsupportedMediaEvidence);
}
