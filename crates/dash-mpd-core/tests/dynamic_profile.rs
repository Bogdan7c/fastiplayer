use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{
    DASH_DIRECT_UTC_SCHEME, DASH_HTTP_XSDATE_UTC_SCHEME, DashAddressing, DashColorMetadata,
    DashDynamicMpd, DashDynamicMpdError, DashDynamicProfileExclusion, DashHdrTransfer,
    DashMpdLimits, DashMpdParseRequest, DashPresentationDuration, DashUtcTimestamp,
    DashUtcTimestampParseError, DashUtcTiming, parse_dynamic_dash_mpd,
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
fn dash_if_simple_http_xsdate_open_period_shape_is_preserved() {
    let document = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
        xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
        xsi:schemaLocation="urn:mpeg:dash:schema:mpd:2011 DASH-MPD.xsd" type="dynamic"
        profiles="urn:mpeg:dash:profile:isoff-live:2011,http://dashif.org/guidelines/dash-if-simple"
        availabilityStartTime="1970-01-01T00:00:00Z"
        publishTime="2026-08-10T10:00:00Z" minimumUpdatePeriod="PT2S"
        timeShiftBufferDepth="PT60S" suggestedPresentationDelay="PT6S">
      <ProgramInformation moreInformationURL="https://example.invalid/info">
        <Title>Bounded informational metadata</Title>
      </ProgramInformation>
      <Period id="P0" start="PT0S">
        <AdaptationSet contentType="video" mimeType="video/mp4" codecs="avc1.4d401f"
            par="16:9" minWidth="640" maxWidth="640" minHeight="360" maxHeight="360"
            maxFrameRate="60/2" segmentAlignment="true" startWithSAP="1">
          <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"></Role>
          <SegmentTemplate timescale="1" initialization="init-$RepresentationID$.mp4"
              media="$RepresentationID$-$Time$.m4s">
            <SegmentTimeline><S t="1786355990" d="2" r="4"></S></SegmentTimeline>
          </SegmentTemplate>
          <Representation id="video" bandwidth="1000000" width="640" height="360"
              sar="1:1" frameRate="60/2"></Representation>
        </AdaptationSet>
      </Period>
      <UTCTiming schemeIdUri="urn:mpeg:dash:utc:http-xsdate:2014" value="clock"></UTCTiming>
    </MPD>"#;

    let mpd = parse(document).expect("livesim2-style strict dynamic snapshot");
    assert_eq!(
        mpd.presentation.media_presentation_duration,
        DashPresentationDuration::OpenEnded
    );
    assert_eq!(
        mpd.presentation.periods[0].duration,
        DashPresentationDuration::OpenEnded
    );
    assert_eq!(mpd.presentation.periods[0].adaptation_sets[0].id, None);
    let DashUtcTiming::HttpXsDate(resource) = &mpd.utc_timing else {
        panic!("ожидался pure HTTP XSDATE descriptor");
    };
    assert_eq!(resource.reference(), "clock");
    assert_eq!(mpd.direct_utc_time(), None);

    let unsupported_profile = document.replace(
        "http://dashif.org/guidelines/dash-if-simple",
        "urn:example:unsupported-profile",
    );
    assert!(matches!(
        parse(&unsupported_profile),
        Err(DashDynamicMpdError::ProfileExcluded(
            DashDynamicProfileExclusion::UnsupportedDeclaredProfile
        ))
    ));
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

/// Diagnostics dynamic snapshot-а не должны раскрывать locator или wall-clock payload.
#[test]
fn dynamic_debug_is_structural_and_redacts_source_material() {
    let secret_locator = "https://media.invalid/private/manifest?token=secret";
    let document = fixture("", "").replace(
        r#"          <Period id="p0""#,
        &format!(
            r#"          <BaseURL>{secret_locator}</BaseURL>
          <Period id="p0""#
        ),
    );

    let mpd = parse(&document).expect("dynamic fixture с root BaseURL");
    let diagnostics = format!("{mpd:?}");

    assert!(diagnostics.contains("period_count: 1"));
    assert!(diagnostics.contains("minimum_update_period_milliseconds: 2000"));
    assert!(diagnostics.contains("suggested_presentation_delay_milliseconds: 6000"));
    assert!(!diagnostics.contains(secret_locator));
    assert!(!diagnostics.contains("2026-07-24T10:01:01Z"));
}

/// Clock boundary принимает один XSDATE timestamp и не раскрывает clock material в diagnostics.
#[test]
fn utc_clock_response_and_diagnostics_are_exact_and_secret_safe() {
    let direct_mpd = parse(&fixture("", "")).expect("direct UTC fixture");
    let direct_timestamp = direct_mpd.direct_utc_time().expect("direct timestamp");
    let parsed_response =
        DashUtcTimestamp::parse_xs_datetime_response(b" \n2026-07-24T10:01:01Z\t")
            .expect("bounded XSDATE response");
    assert_eq!(parsed_response, direct_timestamp);
    assert_eq!(
        format!("{parsed_response:?}"),
        "DashUtcTimestamp(<redacted>)"
    );
    assert_eq!(
        format!("{:?}", direct_mpd.utc_timing),
        "DashUtcTiming::Direct(<redacted>)"
    );

    let http_document = fixture("", "")
        .replace(DASH_DIRECT_UTC_SCHEME, DASH_HTTP_XSDATE_UTC_SCHEME)
        .replace(
            r#"value="2026-07-24T10:01:01Z""#,
            r#"value="clock/private""#,
        );
    let http_mpd = parse(&http_document).expect("HTTP XSDATE descriptor");
    let DashUtcTiming::HttpXsDate(resource) = &http_mpd.utc_timing else {
        panic!("ожидался HTTP XSDATE descriptor");
    };
    assert_eq!(
        format!("{:?}", http_mpd.utc_timing),
        "DashUtcTiming::HttpXsDate(<redacted>)"
    );
    assert_eq!(format!("{resource:?}"), "DashUtcTimingResource(<redacted>)");
    assert!(!format!("{resource:?}").contains(resource.reference()));

    assert_eq!(
        DashUtcTimestamp::parse_xs_datetime_response(&[0xff]),
        Err(DashUtcTimestampParseError::InvalidEncoding)
    );
    assert_eq!(
        DashUtcTimestamp::parse_xs_datetime_response(b" \r\n\t"),
        Err(DashUtcTimestampParseError::InvalidTimestamp)
    );
    assert_eq!(
        DashUtcTimestamp::parse_xs_datetime_response(b"not-a-timestamp"),
        Err(DashUtcTimestampParseError::InvalidTimestamp)
    );
}

/// Typed model queries не смешивают live/finite и SDR/HDR semantic states.
#[test]
fn presentation_and_color_queries_preserve_semantic_distinctions() {
    for (transfer_characteristics, expected) in [
        (Some(16), Some(DashHdrTransfer::Pq)),
        (Some(18), Some(DashHdrTransfer::Hlg)),
        (Some(1), None),
        (None, None),
    ] {
        let color = DashColorMetadata {
            transfer_characteristics,
            ..DashColorMetadata::default()
        };
        assert_eq!(color.hdr_transfer(), expected);
    }

    let finite = DashPresentationDuration::FiniteMilliseconds(12_345);
    assert_eq!(finite.finite_milliseconds(), Some(12_345));
    assert!(!finite.is_open_ended());
    assert_eq!(
        DashPresentationDuration::OpenEnded.finite_milliseconds(),
        None
    );
    assert!(DashPresentationDuration::OpenEnded.is_open_ended());
}

/// Root timing contract fail-closed различает profile exclusions и schema failures.
#[test]
fn root_timing_and_shape_failures_keep_typed_boundaries() {
    let valid = fixture("", "");
    let cases = [
        (
            valid.replace(r#"type="dynamic""#, r#"type="static""#),
            DashDynamicProfileExclusion::NotDynamic,
        ),
        (
            valid.replace(r#" availabilityStartTime="2026-07-24T10:00:00Z""#, ""),
            DashDynamicProfileExclusion::MissingOrInvalidAvailabilityStartTime,
        ),
        (
            valid.replace(
                r#"availabilityStartTime="2026-07-24T10:00:00Z""#,
                r#"availabilityStartTime="not-a-timestamp""#,
            ),
            DashDynamicProfileExclusion::MissingOrInvalidAvailabilityStartTime,
        ),
        (
            valid.replace(r#" publishTime="2026-07-24T10:01:00.250Z""#, ""),
            DashDynamicProfileExclusion::MissingPublishTime,
        ),
        (
            valid.replace(
                r#"minimumUpdatePeriod="PT2S""#,
                r#"minimumUpdatePeriod="PT0S""#,
            ),
            DashDynamicProfileExclusion::MissingOrInvalidMinimumUpdatePeriod,
        ),
        (
            valid.replace(
                r#"suggestedPresentationDelay="PT6S""#,
                r#"suggestedPresentationDelay="PT0S""#,
            ),
            DashDynamicProfileExclusion::MissingOrInvalidSuggestedPresentationDelay,
        ),
        (
            fixture(r#"timeShiftBufferDepth="PT6S""#, ""),
            DashDynamicProfileExclusion::MissingOrInvalidSuggestedPresentationDelay,
        ),
        (
            fixture(r#"unknownTiming="true""#, ""),
            DashDynamicProfileExclusion::UnsupportedTimingConstruct,
        ),
        (
            valid.replace(
                "          <Period",
                "          <Location>next.mpd</Location>\n          <Period",
            ),
            DashDynamicProfileExclusion::UnsupportedTimingConstruct,
        ),
    ];

    for (document, expected) in cases {
        let error = parse(&document).expect_err("неполный dynamic contract должен fail closed");
        assert!(matches!(
            error,
            DashDynamicMpdError::ProfileExcluded(actual) if actual == expected
        ));
    }
}

/// Period bounds являются semantic identity live timeline и не нормализуются догадками.
#[test]
fn dynamic_period_continuity_accepts_derived_bounds_and_rejects_ambiguity() {
    let derived_first_duration = fixture("", "")
        .replace(
            r#"<Period id="p0" start="PT0S" duration="PT120S">"#,
            r#"<Period id="p0" start="PT0S">"#,
        )
        .replace(
            "</Period>\n        </MPD>",
            r#"</Period>
          <Period id="p1" start="PT120S">
            <AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
              <SegmentTemplate timescale="1000" media="tail-$Time$.m4s"
                  initialization="tail-init.mp4">
                <SegmentTimeline><S t="120000" d="2000" r="4"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="tail-video"/>
            </AdaptationSet>
          </Period>
        </MPD>"#,
        );
    let mpd = parse(&derived_first_duration).expect("next Period start задаёт finite bound");
    assert_eq!(
        mpd.presentation.periods[0].duration,
        DashPresentationDuration::FiniteMilliseconds(120_000)
    );
    assert_eq!(
        mpd.presentation.media_presentation_duration,
        DashPresentationDuration::OpenEnded
    );

    let non_contiguous = explicit_two_period_document_with_second_start("PT61S");
    let ambiguous_cases = [
        fixture("", "").replace(r#"duration="PT120S""#, r#"duration="PT0S""#),
        fixture("", "").replace(r#" start="PT0S""#, ""),
        non_contiguous,
    ];
    for (case_index, document) in ambiguous_cases.into_iter().enumerate() {
        let error = parse(&document).expect_err("ambiguous Period timing должен fail closed");
        assert!(
            matches!(
                &error,
                DashDynamicMpdError::ProfileExcluded(
                    DashDynamicProfileExclusion::MissingPeriodTiming
                )
            ),
            "unexpected Period error for case {case_index}: {error:?}"
        );
    }
}

/// Строит explicit two-period snapshot для проверки exact соседних bounds.
fn explicit_two_period_document_with_second_start(second_start: &str) -> String {
    fixture("", "")
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
            &format!(
                r#"</Period>
          <Period id="p1" start="{second_start}" duration="PT60S">
            <AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
              <SegmentTemplate timescale="1000" media="v2-$Time$.m4s"
                  initialization="v2-init.mp4">
                <SegmentTimeline><S t="60000" d="2000" r="29"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="video-2"/>
            </AdaptationSet>
          </Period>
        </MPD>"#
            ),
        )
}

/// LL availability на stable root запрещена так же строго, как на representation template.
#[test]
fn partial_root_base_url_is_rejected_before_runtime_handoff() {
    let document = fixture("", "").replace(
        "          <Period",
        r#"          <BaseURL availabilityTimeComplete="false">video/</BaseURL>
          <Period"#,
    );

    assert!(matches!(
        parse(&document),
        Err(DashDynamicMpdError::ProfileExcluded(
            DashDynamicProfileExclusion::PartialSegmentAvailability
        ))
    ));
}
