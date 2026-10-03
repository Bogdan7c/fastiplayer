use super::*;

fn setting_ids(setting_names: &[&str]) -> Vec<SettingId> {
    setting_names.iter().copied().map(SettingId::from).collect()
}

#[test]
fn recovery_policy_contracts_report_in_place_application() {
    let affected_settings = setting_ids(&[
        "web_media.vod_endpoint_recovery_enabled",
        "web_media.vod_endpoint_recovery_max_consecutive_attempts",
        "web_media.vod_endpoint_recovery_initial_backoff_ms",
        "web_media.vod_endpoint_recovery_max_backoff_ms",
        "web_media.vod_endpoint_recovery_stable_reset_ms",
    ]);

    assert_eq!(
        media_service_apply_mechanism(&affected_settings)
            .expect("recovery policy contracts должны иметь app-report mapping"),
        ApplyMechanism::InPlace
    );
}

#[test]
fn preferred_height_alone_and_mixed_with_policy_report_pipeline_rebuild() {
    let preferred_height = setting_ids(&["web_media.preferred_video_height"]);
    let mixed = setting_ids(&[
        "web_media.vod_endpoint_recovery_enabled",
        "web_media.preferred_video_height",
        "web_media.vod_endpoint_recovery_initial_backoff_ms",
    ]);

    assert_eq!(
        media_service_apply_mechanism(&preferred_height)
            .expect("preferred height contract должен иметь app-report mapping"),
        ApplyMechanism::PipelineRebuild
    );
    assert_eq!(
        media_service_apply_mechanism(&mixed)
            .expect("mixed MediaService contracts должны иметь app-report mapping"),
        ApplyMechanism::PipelineRebuild
    );
}
