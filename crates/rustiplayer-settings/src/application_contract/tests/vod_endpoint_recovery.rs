use super::*;

#[test]
fn descriptors_have_the_exact_media_open_policy_contract() {
    for setting_id in [
        "yt_dlp.vod_endpoint_recovery_enabled",
        "yt_dlp.vod_endpoint_recovery_max_consecutive_attempts",
        "yt_dlp.vod_endpoint_recovery_initial_backoff_ms",
        "yt_dlp.vod_endpoint_recovery_max_backoff_ms",
        "yt_dlp.vod_endpoint_recovery_stable_reset_ms",
    ] {
        let contract = setting_application_contract(&SettingId::from(setting_id))
            .expect("VOD endpoint recovery contract должен существовать");

        assert_eq!(
            contract,
            SettingApplicationContract {
                setting_id: SettingId::from(setting_id),
                route: AppRuntimeRoute::MediaService,
                state_owner: SettingStateOwner::MediaOpenPolicy,
                mechanism: SettingApplyMechanism::PolicyUpdateInPlace,
                rollback_owner: SettingStateOwner::MediaOpenPolicy,
                focused_tests: POLICY_TESTS,
            }
        );
        assert!(
            contract
                .focused_tests
                .contains(&SettingApplyTestScenario::EffectOnNextNaturalEvent)
        );
    }
}
