use crate::{
    SmoothCustomAttribute, SmoothCustomAttributeName, SmoothCustomAttributeSet,
    SmoothCustomAttributeValue, SmoothCustomAttributesRender, SmoothFragmentUrlRenderContext,
    SmoothFragmentUrlTemplate, SmoothManifestError, SmoothUrlTemplateError,
};

use crate::tests_support::{limits, limits_builder};

#[test]
fn accepted_placeholder_spellings_render_exact_relative_paths() {
    let configured_limits = limits();
    let spellings = [
        (
            "QualityLevels({bitrate})/Fragments(video={start time})",
            "QualityLevels(128000)/Fragments(video=42)",
        ),
        (
            "QualityLevels({Bitrate})/Fragments(video={start_time})",
            "QualityLevels(128000)/Fragments(video=42)",
        ),
    ];

    for (template, expected) in spellings {
        let compiled = SmoothFragmentUrlTemplate::parse(template, &configured_limits)
            .expect("standard spelling должен пройти");
        let rendered = compiled
            .render_fragment_path(SmoothFragmentUrlRenderContext::new(
                128_000,
                42,
                SmoothCustomAttributesRender::Unavailable,
            ))
            .expect("template без CustomAttributes должен render-иться");
        assert_eq!(rendered, expected);
    }
}

#[test]
fn standard_custom_attributes_use_typed_bounded_render_input() {
    let configured_limits = limits();
    let attributes = SmoothCustomAttributeSet::new(
        vec![
            SmoothCustomAttribute::new(
                SmoothCustomAttributeName::new("lang", &configured_limits).expect("name валидно"),
                SmoothCustomAttributeValue::new("en-US", &configured_limits)
                    .expect("value валидно"),
            ),
            SmoothCustomAttribute::new(
                SmoothCustomAttributeName::new("role", &configured_limits).expect("name валидно"),
                SmoothCustomAttributeValue::new("main", &configured_limits).expect("value валидно"),
            ),
        ],
        &configured_limits,
    )
    .expect("attribute set валиден");
    let compiled = SmoothFragmentUrlTemplate::parse(
        "QualityLevels({bitrate},{CustomAttributes})/Fragments(audio={start time})",
        &configured_limits,
    )
    .expect("CustomAttributes placeholder валиден");

    assert_eq!(
        compiled
            .render_fragment_path(SmoothFragmentUrlRenderContext::new(
                96_000,
                7,
                SmoothCustomAttributesRender::Values(&attributes),
            ))
            .expect("typed attributes render-ятся"),
        "QualityLevels(96000,lang=en-US,role=main)/Fragments(audio=7)"
    );
    assert_template_error(
        compiled.render_fragment_path(SmoothFragmentUrlRenderContext::new(
            96_000,
            7,
            SmoothCustomAttributesRender::Unavailable,
        )),
        SmoothUrlTemplateError::CustomAttributesUnavailable,
    );
}

#[test]
fn unknown_missing_duplicate_and_unterminated_placeholders_are_rejected() {
    let configured_limits = limits();
    let cases = [
        (
            "x/{unknown}/{bitrate}/{start time}",
            SmoothUrlTemplateError::UnknownPlaceholder,
        ),
        (
            "x/{start time}",
            SmoothUrlTemplateError::MissingBitratePlaceholder,
        ),
        (
            "x/{bitrate}",
            SmoothUrlTemplateError::MissingStartTimePlaceholder,
        ),
        (
            "x/{bitrate}/{Bitrate}/{start time}",
            SmoothUrlTemplateError::DuplicatePlaceholder,
        ),
        (
            "x/{bitrate}/{start time",
            SmoothUrlTemplateError::UnterminatedPlaceholder,
        ),
        (
            "x/}/{bitrate}/{start time}",
            SmoothUrlTemplateError::UnterminatedPlaceholder,
        ),
    ];

    for (template, expected) in cases {
        assert_template_error(
            SmoothFragmentUrlTemplate::parse(template, &configured_limits),
            expected,
        );
    }
}

#[test]
fn absolute_query_fragment_backslash_and_traversal_patterns_fail_closed() {
    let configured_limits = limits();
    let cases = [
        (
            "https://example.invalid/{bitrate}/{start time}",
            SmoothUrlTemplateError::AbsoluteReference,
        ),
        (
            "/root/{bitrate}/{start time}",
            SmoothUrlTemplateError::AbsoluteReference,
        ),
        (
            "x/{bitrate}/{start time}?token=secret",
            SmoothUrlTemplateError::QueryOrFragment,
        ),
        (
            "x/{bitrate}/{start time}#fragment",
            SmoothUrlTemplateError::QueryOrFragment,
        ),
        (
            "x\\{bitrate}\\{start time}",
            SmoothUrlTemplateError::Backslash,
        ),
        (
            "../x/{bitrate}/{start time}",
            SmoothUrlTemplateError::Traversal,
        ),
        (
            "%2e%2e/x/{bitrate}/{start time}",
            SmoothUrlTemplateError::Traversal,
        ),
    ];

    for (template, expected) in cases {
        assert_template_error(
            SmoothFragmentUrlTemplate::parse(template, &configured_limits),
            expected,
        );
    }
}

#[test]
fn template_and_rendered_path_budgets_are_both_enforced() {
    let configured_limits = limits_builder()
        .maximum_template_bytes(40)
        .build()
        .expect("test limits валидны");
    assert_template_error(
        SmoothFragmentUrlTemplate::parse(
            "this-template-is-too-long/{bitrate}/{start time}",
            &configured_limits,
        ),
        SmoothUrlTemplateError::TooLong,
    );

    let compiled = SmoothFragmentUrlTemplate::parse("x/{bitrate}/{start time}", &configured_limits)
        .expect("короткий template валиден");
    assert_template_error(
        compiled.render_fragment_path(SmoothFragmentUrlRenderContext::new(
            u64::MAX,
            u64::MAX,
            SmoothCustomAttributesRender::Unavailable,
        )),
        SmoothUrlTemplateError::RenderedPathTooLong,
    );
}

#[test]
fn template_debug_and_errors_never_echo_secret_input() {
    let configured_limits = limits();
    let secret = "secret-account-path";
    let compiled = SmoothFragmentUrlTemplate::parse(
        &format!("{secret}/{{bitrate}}/{{start time}}"),
        &configured_limits,
    )
    .expect("relative template валиден");
    assert!(!format!("{compiled:?}").contains(secret));

    let error = SmoothFragmentUrlTemplate::parse(
        &format!("{secret}?token=do-not-log/{{bitrate}}/{{start time}}"),
        &configured_limits,
    )
    .expect_err("query должен быть отклонён");
    assert!(!format!("{error:?}").contains(secret));
    assert!(!error.to_string().contains(secret));
}

fn assert_template_error<T: std::fmt::Debug>(
    result: Result<T, SmoothManifestError>,
    expected: SmoothUrlTemplateError,
) {
    assert_eq!(
        result.expect_err("template fixture должна завершиться ошибкой"),
        SmoothManifestError::InvalidUrlTemplate { reason: expected }
    );
}
