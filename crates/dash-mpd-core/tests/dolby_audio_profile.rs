//! MPD с AC-3/E-AC-3 audio: codecs `ac-3`/`ec-3` и Dolby `AudioChannelConfiguration`.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{
    DashAudioChannelConfiguration, DashContainer, DashMediaKind, DashMpd, DashMpdError,
    DashMpdErrorKind, DashMpdLimits, DashMpdParseRequest, parse_dash_mpd,
};

/// Test-only XML budgets; production defaults намеренно отсутствуют.
fn xml_budgets() -> XmlBudgets {
    XmlBudgets::builder()
        .maximum_document_bytes(64 * 1024)
        .maximum_depth(32)
        .maximum_tokens(1_024)
        .maximum_attributes_per_element(32)
        .maximum_attribute_count(512)
        .maximum_attribute_bytes(32 * 1024)
        .maximum_namespace_declarations_per_element(8)
        .maximum_namespace_declaration_count(32)
        .maximum_namespace_bytes(4 * 1024)
        .maximum_text_bytes(32 * 1024)
        .build()
        .expect("The full XML budget is defined")
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

fn parse(document: &str) -> Result<DashMpd, DashMpdError> {
    parse_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: limits(),
    })
}

/// Audio AdaptationSet с одной Representation и заданной channel configuration.
fn audio_mpd(codecs: &str, scheme: &str, value: &str) -> String {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT2S">
          <Period duration="PT2S">
            <AdaptationSet mimeType="audio/mp4" codecs="{codecs}" lang="en" audioSamplingRate="48000">
              <AudioChannelConfiguration schemeIdUri="{scheme}" value="{value}"/>
              <SegmentTemplate initialization="init.mp4" media="$Number$.m4s" duration="2" startNumber="1"/>
              <Representation id="dolby" bandwidth="384000"/>
            </AdaptationSet>
          </Period>
        </MPD>"#
    )
}

const DOLBY_TAG_SCHEME: &str = "tag:dolby.com,2014:dash:audio_channel_configuration:2011";
const DOLBY_URN_SCHEME: &str = "urn:dolby:dash:audio_channel_configuration:2011";

/// `ec-3` 5.1 и `ac-3` stereo принимаются как ISO-BMFF audio с точной Dolby-маской.
#[test]
fn dolby_codecs_and_both_channel_schemes_parse_to_exact_mask() {
    for (codecs, scheme, value, channels) in [
        ("ec-3", DOLBY_TAG_SCHEME, "F801", 6),
        ("ac-3", DOLBY_URN_SCHEME, "A000", 2),
        ("ec-3", DOLBY_URN_SCHEME, "FA01", 8),
    ] {
        let mpd = parse(&audio_mpd(codecs, scheme, value))
            .unwrap_or_else(|error| panic!("{codecs}/{value} must parse: {error:?}"));
        let representation = &mpd.periods[0].adaptation_sets[0].representations[0];
        assert_eq!(representation.codecs, codecs);
        assert_eq!(representation.container, DashContainer::IsoBmff);
        assert_eq!(representation.media_kind, DashMediaKind::Audio);
        let Some(DashAudioChannelConfiguration::DolbyChannelMask(mask)) =
            representation.audio_channel_configuration
        else {
            panic!("Dolby scheme must produce a channel mask");
        };
        assert_eq!(mask.channel_count(), channels, "{value}");
    }
}

/// Невалидная Dolby-маска не роняет MPD (прежняя семантика неизвестной схемы),
/// а AC-3 в WebM по-прежнему не является поддерживаемой формой.
#[test]
fn malformed_dolby_mask_stays_unsupported_and_webm_dolby_is_rejected() {
    let mpd = parse(&audio_mpd("ec-3", DOLBY_TAG_SCHEME, "F80"))
        .expect("malformed Dolby mask must not fail the whole MPD");
    assert_eq!(
        mpd.periods[0].adaptation_sets[0].representations[0].audio_channel_configuration,
        Some(DashAudioChannelConfiguration::Unsupported)
    );

    let webm = audio_mpd("ac-3", DOLBY_TAG_SCHEME, "A000").replace("audio/mp4", "audio/webm");
    assert_eq!(
        parse(&webm).expect_err("AC-3 is not a WebM codec").kind(),
        DashMpdErrorKind::UnsupportedMediaEvidence
    );
}
