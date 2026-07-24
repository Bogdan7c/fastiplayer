use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{
    DashAddressing, DashDynamicMpd, DashDynamicMpdError, DashDynamicProfileExclusion,
    DashMpdLimits, DashMpdParseRequest, parse_dynamic_dash_mpd,
};

/// Test-only hardened XML budget.
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
        .expect("test XML budgets валидны")
}

/// Test-only schema caps.
fn limits() -> DashMpdLimits {
    DashMpdLimits {
        maximum_periods: 8,
        maximum_adaptation_sets_per_period: 8,
        maximum_representations_per_adaptation_set: 8,
        maximum_segments_per_list: 32,
        maximum_timeline_entries: 64,
        maximum_schema_string_bytes: 4 * 1024,
    }
}

/// Разбирает dynamic fixture через production entry point.
fn parse(document: &str) -> Result<DashDynamicMpd, DashDynamicMpdError> {
    parse_dynamic_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: limits(),
    })
}

/// Строит минимальный exact dynamic MPD с заменяемыми root/body fragments.
fn fixture(root_extra: &str, template_extra: &str) -> String {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
            availabilityStartTime="2026-07-24T10:00:00Z"
            publishTime="2026-07-24T10:01:00.250Z"
            minimumUpdatePeriod="PT2S"
            suggestedPresentationDelay="PT6S" {root_extra}>
          <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014"
              value="2026-07-24T10:01:01Z"/>
          <Period id="p0" start="PT0S" duration="PT120S">
            <AdaptationSet id="v" mimeType="video/mp4" codecs="avc1.4d401f">
              <SegmentTemplate timescale="1000" media="v-$Time$.m4s"
                  initialization="v-init.mp4" {template_extra}>
                <SegmentTimeline><S t="0" d="2000" r="59"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="video"/>
            </AdaptationSet>
          </Period>
        </MPD>"#
    )
}

#[test]
fn direct_utc_required_fields_and_missing_tsb_default_are_preserved() {
    let mpd = parse(&fixture("", "")).expect("strict dynamic fixture");
    assert_eq!(mpd.minimum_update_period_milliseconds, 2_000);
    assert_eq!(mpd.suggested_presentation_delay_milliseconds, 6_000);
    assert_eq!(mpd.time_shift_buffer_depth_milliseconds, None);
    assert_eq!(
        mpd.publish_time.unix_nanoseconds(),
        1_784_887_260_250_000_000
    );
}

#[test]
fn timing_depth_ato_and_atc_true_are_parsed_without_float_rounding() {
    let mpd = parse(&fixture(
        r#"timeShiftBufferDepth="PT30S""#,
        r#"availabilityTimeOffset="1.000000007" availabilityTimeComplete="true""#,
    ))
    .expect("finite complete availability");
    assert_eq!(mpd.time_shift_buffer_depth_milliseconds, Some(30_000));
    let DashAddressing::Template(template) =
        &mpd.presentation.periods[0].adaptation_sets[0].representations[0].addressing
    else {
        panic!("dynamic profile допускает только template");
    };
    assert_eq!(
        template.availability_time_offset_nanoseconds,
        Some(1_000_000_007)
    );
    assert_eq!(template.availability_time_complete, Some(true));
}

#[test]
fn unsupported_clock_missing_timing_and_partial_availability_are_typed() {
    let cases = [
        (
            fixture("", "").replace(
                "urn:mpeg:dash:utc:direct:2014",
                "urn:mpeg:dash:utc:http-iso:2014",
            ),
            DashDynamicProfileExclusion::UnsupportedClockModel,
        ),
        (
            fixture("", "").replace(" minimumUpdatePeriod=\"PT2S\"", ""),
            DashDynamicProfileExclusion::MissingOrInvalidMinimumUpdatePeriod,
        ),
        (
            fixture("", "").replace(" suggestedPresentationDelay=\"PT6S\"", ""),
            DashDynamicProfileExclusion::MissingOrInvalidSuggestedPresentationDelay,
        ),
        (
            fixture("", r#"availabilityTimeComplete="false""#),
            DashDynamicProfileExclusion::PartialSegmentAvailability,
        ),
        (
            fixture("", r#"availabilityTimeOffset="INF""#),
            DashDynamicProfileExclusion::PartialSegmentAvailability,
        ),
    ];
    for (document, expected) in cases {
        let DashDynamicMpdError::ProfileExcluded(actual) =
            parse(&document).expect_err("profile должен fail closed")
        else {
            panic!("ожидался typed profile exclusion");
        };
        assert_eq!(actual, expected);
    }
}

#[test]
fn duration_addressing_and_missing_period_identity_are_excluded() {
    let duration_mode = fixture("", "")
        .replace(
            r#"<SegmentTimeline><S t="0" d="2000" r="59"/></SegmentTimeline>"#,
            "",
        )
        .replace(
            r#"initialization="v-init.mp4" >"#,
            r#"initialization="v-init.mp4" duration="2000">"#,
        );
    let missing_identity = fixture("", "").replace(r#" id="p0""#, "");
    for (document, expected) in [
        (
            duration_mode,
            DashDynamicProfileExclusion::UnsupportedAddressing,
        ),
        (
            missing_identity,
            DashDynamicProfileExclusion::MissingPeriodTiming,
        ),
    ] {
        let DashDynamicMpdError::ProfileExcluded(actual) =
            parse(&document).expect_err("неполный timing contract")
        else {
            panic!("ожидался typed profile exclusion");
        };
        assert_eq!(actual, expected);
    }
}

#[test]
fn explicit_contiguous_multi_period_live_is_preserved() {
    let document = fixture("", "")
        .replace(
            r#"<Period id="p0" start="PT0S" duration="PT120S">"#,
            r#"<Period id="p0" start="PT0S" duration="PT60S">"#,
        )
        .replace(
            r#"<SegmentTimeline><S t="0" d="2000" r="59"/></SegmentTimeline>"#,
            r#"<SegmentTimeline><S t="0" d="2000" r="29"/></SegmentTimeline>"#,
        )
        .replace(
            "</Period>\n        </MPD>",
            r#"</Period>
          <Period id="p1" start="PT60S" duration="PT60S">
            <AdaptationSet id="v" mimeType="video/mp4" codecs="avc1.4d401f">
              <SegmentTemplate timescale="1000" presentationTimeOffset="60000"
                  media="v2-$Time$.m4s"
                  initialization="v2-init.mp4">
                <SegmentTimeline><S t="60000" d="2000" r="29"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="video"/>
            </AdaptationSet>
          </Period>
        </MPD>"#,
        );
    let mpd = parse(&document).expect("explicit multi-period live");
    assert_eq!(mpd.presentation.periods.len(), 2);
    assert_eq!(mpd.presentation.periods[1].start_milliseconds, 60_000);
}

#[test]
fn utc_timing_accepts_paired_whitespace_and_rejects_payload_nested_or_duplicate_forms() {
    let paired = fixture("", "").replace(
        r#"<UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014"
              value="2026-07-24T10:01:01Z"/>"#,
        r#"<UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014"
              value="2026-07-24T10:01:01Z">
          </UTCTiming>"#,
    );
    parse(&paired).expect("paired whitespace-only UTCTiming входит в strict profile");

    let text_payload = paired.replace(
        "\n          </UTCTiming>",
        "\n            forbidden\n          </UTCTiming>",
    );
    let nested_payload = paired.replace(
        "\n          </UTCTiming>",
        "\n            <Clock/>\n          </UTCTiming>",
    );
    let duplicate_mixed = paired.replace(
        "\n          <Period",
        r#"
          <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014"
              value="2026-07-24T10:01:01Z"/>
          <Period"#,
    );
    for document in [text_payload, nested_payload, duplicate_mixed] {
        let error = parse(&document).expect_err("неоднозначный UTC contract должен fail closed");
        assert!(matches!(
            error,
            DashDynamicMpdError::ProfileExcluded(
                DashDynamicProfileExclusion::UnsupportedClockModel
            )
        ));
    }
}
