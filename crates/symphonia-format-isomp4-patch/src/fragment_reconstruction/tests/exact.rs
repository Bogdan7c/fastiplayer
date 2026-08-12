//! Exact captured video/audio plans и timing/flags accounting.

use super::super::model::{FragmentCompositionOffsetSemantics, FragmentSampleDefaults};
use super::support::{
    AUDIO_FIRST, AUDIO_SECOND, MANIFEST, VIDEO_HIGH_FIRST, VIDEO_HIGH_SECOND, VIDEO_LOW_FIRST,
    audio_expectation, inspect, inspect_with_semantics, limits, video_expectation,
};

#[test]
fn exact_manifest_records_expected_smooth_profile() {
    let manifest = std::str::from_utf8(MANIFEST).expect("captured manifest is UTF-8");
    assert!(manifest.contains("MajorVersion=\"2\""));
    assert!(manifest.contains("MinorVersion=\"0\""));
    assert!(manifest.contains("TimeScale=\"10000000\""));
    assert!(manifest.contains("<c t=\"0\" d=\"39680000\" />"));
    assert!(manifest.contains("<c t=\"0\" d=\"40000000\" />"));
}

#[test]
fn exact_video_renditions_build_canonical_first_fragment_plans() {
    for (fragment, expected_samples) in [(VIDEO_LOW_FIRST, 96), (VIDEO_HIGH_FIRST, 96)] {
        let plan = inspect(
            fragment,
            video_expectation(0, FragmentSampleDefaults::absent()),
        )
        .expect("captured video fragment is accepted");

        assert_eq!(plan.sequence_number(), 1);
        assert_eq!(plan.track_id().get(), 1);
        assert_eq!(plan.base_decode_time(), 0);
        assert_eq!(plan.coded_coverage().start(), 0);
        assert_eq!(plan.coded_coverage().end_exclusive(), 40_000_000);
        assert_eq!(plan.coded_coverage().duration(), 40_000_000);
        assert_eq!(plan.samples().len(), expected_samples);
        assert_eq!(plan.samples().first().expect("first sample").dts(), 0);
        assert_eq!(
            plan.samples().last().expect("last sample").dts()
                + u64::from(plan.samples().last().expect("last sample").duration()),
            40_000_000
        );
        assert_payload_accounting(&plan);
    }
}

#[test]
fn exact_piff_video_fragment_restores_signed_composition_offsets() {
    let plan = inspect(
        VIDEO_HIGH_SECOND,
        video_expectation(40_000_000, FragmentSampleDefaults::absent()),
    )
    .expect("captured second video fragment is accepted");

    assert_eq!(plan.sequence_number(), 2);
    assert_eq!(plan.samples().len(), 96);
    assert!(
        plan.samples()
            .iter()
            .any(|sample| sample.composition_offset() < 0)
    );
    for sample in plan.samples() {
        assert_eq!(
            i128::from(sample.pts()),
            i128::from(sample.dts()) + i128::from(sample.composition_offset())
        );
    }
    assert_payload_accounting(&plan);
}

#[test]
fn standard_iso_bmff_version_zero_remains_unsigned() {
    let plan = inspect_with_semantics(
        VIDEO_HIGH_SECOND,
        FragmentCompositionOffsetSemantics::IsoBmffVersioned,
        video_expectation(40_000_000, FragmentSampleDefaults::absent()),
        &limits(),
        &|| false,
    )
    .expect("standard ISO BMFF v0 offsets остаются unsigned");

    assert!(
        plan.samples()
            .iter()
            .any(|sample| sample.composition_offset() > i64::from(i32::MAX))
    );
}

#[test]
fn exact_audio_fragments_build_plans_without_fake_rap_requirement() {
    for (fragment, base, coded_end, sequence, sample_count) in [
        (AUDIO_FIRST, 0, 40_106_666, 1, 188),
        (AUDIO_SECOND, 39_680_000, 79_573_334, 2, 187),
    ] {
        let plan = inspect(
            fragment,
            audio_expectation(base, FragmentSampleDefaults::absent()),
        )
        .expect("captured audio fragment is accepted");

        assert_eq!(plan.sequence_number(), sequence);
        assert_eq!(plan.base_decode_time(), base);
        assert_eq!(plan.coded_coverage().start(), base);
        assert_eq!(plan.coded_coverage().end_exclusive(), coded_end);
        assert_eq!(plan.samples().len(), sample_count);
        assert_payload_accounting(&plan);
    }
}

#[test]
fn plan_debug_never_contains_sample_payload_bytes() {
    let plan = inspect(
        VIDEO_HIGH_FIRST,
        video_expectation(0, FragmentSampleDefaults::absent()),
    )
    .expect("captured video fragment is accepted");
    let first_payload = plan.sample_payload(0).expect("first sample payload");
    let secret_prefix = first_payload
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let debug = format!("{plan:?}");

    assert!(!debug.contains(&secret_prefix));
    assert!(debug.contains("sample_count"));
}

/// Проверяет отсутствие gaps/overlap и точное покрытие `mdat`.
fn assert_payload_accounting(plan: &super::super::model::NormalizedFragmentPlan<'_>) {
    let mdat = plan.mdat_payload_range();
    let first = plan.samples().first().expect("fixture has samples");
    let last = plan.samples().last().expect("fixture has samples");
    assert_eq!(first.payload_range().start, mdat.start);
    assert_eq!(last.payload_range().end, mdat.end);
    for adjacent in plan.samples().windows(2) {
        assert_eq!(
            adjacent[0].payload_range().end,
            adjacent[1].payload_range().start
        );
    }
    let accounted: usize = plan
        .samples()
        .iter()
        .map(|sample| sample.payload_range().len())
        .sum();
    assert_eq!(accounted, mdat.len());
}
