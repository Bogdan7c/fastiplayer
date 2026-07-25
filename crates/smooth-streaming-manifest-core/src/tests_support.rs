use crate::{SmoothManifestLimits, SmoothManifestLimitsBuilder};

pub(crate) fn limits_builder() -> SmoothManifestLimitsBuilder {
    SmoothManifestLimits::builder()
        .maximum_streams(8)
        .maximum_qualities_per_stream(16)
        .maximum_total_qualities(32)
        .maximum_timeline_entries_per_stream(64)
        .maximum_total_timeline_entries(128)
        .maximum_fragments_per_stream(2_000_000)
        .maximum_total_fragments(4_000_000)
        .maximum_template_bytes(512)
        .maximum_string_bytes(256)
        .maximum_codec_bytes(4_096)
        .maximum_custom_attributes_per_quality(8)
        .maximum_total_custom_attributes(32)
        .maximum_custom_attribute_name_bytes(64)
        .maximum_custom_attribute_value_bytes(128)
}

pub(crate) fn limits() -> SmoothManifestLimits {
    limits_builder()
        .build()
        .expect("полный test budget должен быть валиден")
}
