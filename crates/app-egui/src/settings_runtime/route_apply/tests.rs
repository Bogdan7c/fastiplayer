use super::*;

fn setting_ids(setting_names: &[&str]) -> Vec<SettingId> {
    setting_names.iter().copied().map(SettingId::from).collect()
}

#[test]
fn recovery_policy_contracts_report_in_place_application() {
    let affected_settings = setting_ids(&[
        "yt_dlp.vod_endpoint_recovery_enabled",
        "yt_dlp.vod_endpoint_recovery_max_consecutive_attempts",
        "yt_dlp.vod_endpoint_recovery_initial_backoff_ms",
        "yt_dlp.vod_endpoint_recovery_max_backoff_ms",
        "yt_dlp.vod_endpoint_recovery_stable_reset_ms",
    ]);

    assert_eq!(
        media_service_apply_mechanism(&affected_settings)
            .expect("recovery policy contracts должны иметь app-report mapping"),
        ApplyMechanism::InPlace
    );
}

#[test]
fn preferred_height_alone_and_mixed_with_policy_report_pipeline_rebuild() {
    let preferred_height = setting_ids(&["yt_dlp.preferred_video_height"]);
    let mixed = setting_ids(&[
        "yt_dlp.vod_endpoint_recovery_enabled",
        "yt_dlp.preferred_video_height",
        "yt_dlp.vod_endpoint_recovery_initial_backoff_ms",
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

#[test]
fn unknown_and_foreign_route_contracts_fail_closed() {
    let unknown = setting_ids(&["yt_dlp.future_unmapped_policy"]);
    let foreign_route = setting_ids(&["player.start_paused"]);

    assert!(
        media_service_apply_mechanism(&unknown).is_err(),
        "unknown setting не должен молча считаться InPlace"
    );
    assert!(
        media_service_apply_mechanism(&foreign_route).is_err(),
        "contract другого owner route не должен получать MediaService report"
    );
}
