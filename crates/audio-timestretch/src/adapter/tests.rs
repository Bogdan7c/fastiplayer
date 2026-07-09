//! Focused unit tests concrete timestretch probe adapter-а.

use super::*;
use audio_core::{
    AudioTempoChannelCount, AudioTempoPcmFormat, AudioTempoSampleRateHz, AudioTempoSegmentId,
};

fn stereo_48k_format() -> AudioTempoPcmFormat {
    AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(48_000).expect("valid rate"),
        AudioTempoChannelCount::new(2).expect("valid channels"),
    )
}

#[test]
fn default_settings_use_quality_first_balanced_profile() {
    assert_eq!(
        TimestretchTempoSettings::default().quality_mode(),
        TimestretchQualityMode::Balanced
    );
    assert_eq!(
        TimestretchTempoSettings::SESSION_THREAD_DEFAULT.quality_mode(),
        TimestretchQualityMode::Balanced
    );
    assert_eq!(
        TimestretchTempoSettings::REALTIME_DEFAULT.quality_mode(),
        TimestretchQualityMode::LowLatency
    );
}

#[test]
fn project_ratio_is_inverted_for_backend_stretch_ratio() {
    let double_speed = AudioTempoRatio::new(2.0).unwrap();
    let half_speed = AudioTempoRatio::new(0.5).unwrap();

    assert_eq!(backend_stretch_ratio_for_project_ratio(double_speed), 0.5);
    assert_eq!(backend_stretch_ratio_for_project_ratio(half_speed), 2.0);
}

#[test]
fn reset_preserves_active_segment_ratio() {
    let pcm_format = AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(48_000).unwrap(),
        AudioTempoChannelCount::new(2).unwrap(),
    );
    let initial_segment = AudioTempoSegment::new(
        AudioTempoSegmentId::new(1),
        AudioTempoRatio::new(2.0).unwrap(),
    );
    let config = AudioTempoProcessorConfig::new(pcm_format, initial_segment);
    let mut processor = TimestretchTempoProcessor::new(config).unwrap();

    let report = processor.reset().unwrap();

    assert_eq!(report.effective_ratio(), initial_segment.ratio());
    assert_eq!(
        processor.ratio_snapshot().unwrap().target_project_ratio,
        initial_segment.ratio()
    );
}

#[test]
fn factory_creates_neutral_processor_trait_object() {
    let pcm_format = AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(48_000).unwrap(),
        AudioTempoChannelCount::new(2).unwrap(),
    );
    let initial_segment =
        AudioTempoSegment::new(AudioTempoSegmentId::new(1), AudioTempoRatio::NORMAL);
    let config = AudioTempoProcessorConfig::new(pcm_format, initial_segment);
    let factory = TimestretchTempoProcessorFactory::default();

    let mut processor = factory.create_processor(config).unwrap();
    let report = processor.reset().unwrap();

    assert_eq!(report.segment_id(), initial_segment.segment_id());
    assert_eq!(report.effective_ratio(), AudioTempoRatio::NORMAL);
}

#[test]
fn trait_set_segment_updates_target_ratio_without_recreating_processor() {
    let pcm_format = AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(48_000).unwrap(),
        AudioTempoChannelCount::new(2).unwrap(),
    );
    let initial_segment =
        AudioTempoSegment::new(AudioTempoSegmentId::new(1), AudioTempoRatio::NORMAL);
    let updated_segment = AudioTempoSegment::new(
        AudioTempoSegmentId::new(2),
        AudioTempoRatio::new(2.0).unwrap(),
    );
    let config = AudioTempoProcessorConfig::new(pcm_format, initial_segment);
    let mut processor = TimestretchTempoProcessor::new(config).unwrap();

    let report = AudioTempoProcessor::set_segment(&mut processor, updated_segment).unwrap();

    assert_eq!(report.segment_id(), updated_segment.segment_id());
    assert_eq!(
        processor.ratio_snapshot().unwrap().target_project_ratio,
        updated_segment.ratio()
    );
}

#[test]
fn trait_set_segment_backend_rejection_preserves_old_segment_and_ratio() {
    let initial_segment =
        AudioTempoSegment::new(AudioTempoSegmentId::new(21), AudioTempoRatio::NORMAL);
    let mut processor = TimestretchTempoProcessor::new(AudioTempoProcessorConfig::new(
        stereo_48k_format(),
        initial_segment,
    ))
    .expect("processor should be created");
    let ratio_before = processor.ratio_snapshot().expect("ratio snapshot");
    let backend_rejected_ratio =
        AudioTempoRatio::new(f64::from_bits(1)).expect("neutral ratio is positive and finite");
    let rejected_segment =
        AudioTempoSegment::new(AudioTempoSegmentId::new(22), backend_rejected_ratio);

    AudioTempoProcessor::set_segment(&mut processor, rejected_segment)
        .expect_err("inverted backend ratio must be rejected");

    let ratio_after = processor.ratio_snapshot().expect("ratio snapshot");
    assert_eq!(processor.active_segment, initial_segment);
    assert_eq!(
        ratio_after.target_backend_stretch_ratio,
        ratio_before.target_backend_stretch_ratio
    );
}
