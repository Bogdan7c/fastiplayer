use crate::{
    SmoothManifestLimitBuildError, SmoothManifestLimitKind, SmoothManifestLimits,
    SmoothManifestLimitsBuilder,
};

const ALL_LIMITS: [SmoothManifestLimitKind; 14] = [
    SmoothManifestLimitKind::Streams,
    SmoothManifestLimitKind::QualitiesPerStream,
    SmoothManifestLimitKind::TotalQualities,
    SmoothManifestLimitKind::TimelineEntriesPerStream,
    SmoothManifestLimitKind::TotalTimelineEntries,
    SmoothManifestLimitKind::FragmentsPerStream,
    SmoothManifestLimitKind::TotalFragments,
    SmoothManifestLimitKind::TemplateBytes,
    SmoothManifestLimitKind::StringBytes,
    SmoothManifestLimitKind::CodecBytes,
    SmoothManifestLimitKind::CustomAttributesPerQuality,
    SmoothManifestLimitKind::TotalCustomAttributes,
    SmoothManifestLimitKind::CustomAttributeNameBytes,
    SmoothManifestLimitKind::CustomAttributeValueBytes,
];

#[test]
fn every_limit_is_mandatory_and_has_no_hidden_default() {
    for omitted in ALL_LIMITS {
        let error = configured_builder(Some(omitted), None)
            .build()
            .expect_err("каждый budget должен быть обязательным");
        let SmoothManifestLimitBuildError::Missing(missing) = error else {
            panic!("ожидался missing budget, получено {error:?}");
        };
        assert_eq!(missing.field(), omitted);
    }
}

#[test]
fn every_limit_rejects_zero() {
    for zeroed in ALL_LIMITS {
        let error = configured_builder(None, Some(zeroed))
            .build()
            .expect_err("нулевой budget не должен проходить");
        assert_eq!(error, SmoothManifestLimitBuildError::Zero { field: zeroed });
    }
}

#[test]
fn each_per_stream_budget_must_fit_its_total_budget() {
    let pairs = [
        (
            SmoothManifestLimitKind::QualitiesPerStream,
            SmoothManifestLimitKind::TotalQualities,
        ),
        (
            SmoothManifestLimitKind::TimelineEntriesPerStream,
            SmoothManifestLimitKind::TotalTimelineEntries,
        ),
        (
            SmoothManifestLimitKind::FragmentsPerStream,
            SmoothManifestLimitKind::TotalFragments,
        ),
        (
            SmoothManifestLimitKind::CustomAttributesPerQuality,
            SmoothManifestLimitKind::TotalCustomAttributes,
        ),
    ];

    for (per_stream, total) in pairs {
        let error = builder_with_pair_violation(per_stream)
            .build()
            .expect_err("per-stream budget не должен превышать total");
        assert_eq!(
            error,
            SmoothManifestLimitBuildError::PerStreamExceedsTotal { per_stream, total }
        );
    }
}

#[test]
fn complete_builder_preserves_every_named_value() {
    let limits = configured_builder(None, None)
        .build()
        .expect("полный budget валиден");

    assert_eq!(limits.maximum_streams(), 8);
    assert_eq!(limits.maximum_qualities_per_stream(), 16);
    assert_eq!(limits.maximum_total_qualities(), 32);
    assert_eq!(limits.maximum_timeline_entries_per_stream(), 64);
    assert_eq!(limits.maximum_total_timeline_entries(), 128);
    assert_eq!(limits.maximum_fragments_per_stream(), 256);
    assert_eq!(limits.maximum_total_fragments(), 512);
    assert_eq!(limits.maximum_template_bytes(), 512);
    assert_eq!(limits.maximum_string_bytes(), 256);
    assert_eq!(limits.maximum_codec_bytes(), 4_096);
    assert_eq!(limits.maximum_custom_attributes_per_quality(), 8);
    assert_eq!(limits.maximum_total_custom_attributes(), 32);
    assert_eq!(limits.maximum_custom_attribute_name_bytes(), 64);
    assert_eq!(limits.maximum_custom_attribute_value_bytes(), 128);
}

fn configured_builder(
    omitted: Option<SmoothManifestLimitKind>,
    zeroed: Option<SmoothManifestLimitKind>,
) -> SmoothManifestLimitsBuilder {
    let mut builder = SmoothManifestLimits::builder();
    macro_rules! set_limit {
        ($kind:expr, $method:ident, $value:expr) => {
            if omitted != Some($kind) {
                builder = builder.$method(if zeroed == Some($kind) { 0 } else { $value });
            }
        };
    }
    set_limit!(SmoothManifestLimitKind::Streams, maximum_streams, 8);
    set_limit!(
        SmoothManifestLimitKind::QualitiesPerStream,
        maximum_qualities_per_stream,
        16
    );
    set_limit!(
        SmoothManifestLimitKind::TotalQualities,
        maximum_total_qualities,
        32
    );
    set_limit!(
        SmoothManifestLimitKind::TimelineEntriesPerStream,
        maximum_timeline_entries_per_stream,
        64
    );
    set_limit!(
        SmoothManifestLimitKind::TotalTimelineEntries,
        maximum_total_timeline_entries,
        128
    );
    set_limit!(
        SmoothManifestLimitKind::FragmentsPerStream,
        maximum_fragments_per_stream,
        256
    );
    set_limit!(
        SmoothManifestLimitKind::TotalFragments,
        maximum_total_fragments,
        512
    );
    set_limit!(
        SmoothManifestLimitKind::TemplateBytes,
        maximum_template_bytes,
        512
    );
    set_limit!(
        SmoothManifestLimitKind::StringBytes,
        maximum_string_bytes,
        256
    );
    set_limit!(
        SmoothManifestLimitKind::CodecBytes,
        maximum_codec_bytes,
        4_096
    );
    set_limit!(
        SmoothManifestLimitKind::CustomAttributesPerQuality,
        maximum_custom_attributes_per_quality,
        8
    );
    set_limit!(
        SmoothManifestLimitKind::TotalCustomAttributes,
        maximum_total_custom_attributes,
        32
    );
    set_limit!(
        SmoothManifestLimitKind::CustomAttributeNameBytes,
        maximum_custom_attribute_name_bytes,
        64
    );
    set_limit!(
        SmoothManifestLimitKind::CustomAttributeValueBytes,
        maximum_custom_attribute_value_bytes,
        128
    );
    builder
}

fn builder_with_pair_violation(per_stream: SmoothManifestLimitKind) -> SmoothManifestLimitsBuilder {
    let builder = configured_builder(None, None);
    match per_stream {
        SmoothManifestLimitKind::QualitiesPerStream => builder.maximum_qualities_per_stream(33),
        SmoothManifestLimitKind::TimelineEntriesPerStream => {
            builder.maximum_timeline_entries_per_stream(129)
        }
        SmoothManifestLimitKind::FragmentsPerStream => builder.maximum_fragments_per_stream(513),
        SmoothManifestLimitKind::CustomAttributesPerQuality => {
            builder.maximum_custom_attributes_per_quality(33)
        }
        _ => unreachable!("fixture принимает только per-stream kinds"),
    }
}
