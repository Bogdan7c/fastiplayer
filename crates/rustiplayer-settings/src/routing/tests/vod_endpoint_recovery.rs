use super::*;
use rustiplayer_config::{PreferredVideoHeight, YtDlpHdrSelection};

#[test]
fn changes_form_one_exact_media_service_route() {
    let registry = app_config_registry().expect("registry builds");
    let defaults = AppConfig::default();
    let mut previous = defaults.clone();
    previous.network.memory_cache_mb += 7;
    previous.network.connect_timeout_ms += 1_234;
    previous.yt_dlp.enabled = false;
    previous.yt_dlp.hdr_selection = YtDlpHdrSelection::PreferHdrWhenAvailable;
    previous.yt_dlp.preferred_video_height = Some(
        PreferredVideoHeight::new(1_440).expect("sentinel preferred height должна быть valid"),
    );
    previous.yt_dlp.resolve_timeout_ms += 1_111;
    previous.yt_dlp.single_item_stdout_limit_bytes += 4_096;
    previous.yt_dlp.single_item_stderr_limit_bytes += 2_048;
    previous.yt_dlp.single_item_json_node_limit += 17;
    previous
        .validate()
        .expect("non-default sentinel snapshot должен оставаться valid");
    assert_ne!(previous.network, defaults.network);
    assert_ne!(previous.yt_dlp, defaults.yt_dlp);

    let mut recovery_policy = previous.clone();
    recovery_policy.yt_dlp.vod_endpoint_recovery_enabled =
        !recovery_policy.yt_dlp.vod_endpoint_recovery_enabled;
    recovery_policy
        .yt_dlp
        .vod_endpoint_recovery_max_consecutive_attempts += 1;
    recovery_policy
        .yt_dlp
        .vod_endpoint_recovery_initial_backoff_ms += 100;
    recovery_policy.yt_dlp.vod_endpoint_recovery_max_backoff_ms += 100;
    recovery_policy.yt_dlp.vod_endpoint_recovery_stable_reset_ms += 1_000;

    let recovery_diff = registry
        .diff(&previous, &recovery_policy)
        .expect("VOD recovery policy diff succeeds");
    let expected_recovery_settings = vec![
        SettingId::from("yt_dlp.vod_endpoint_recovery_enabled"),
        SettingId::from("yt_dlp.vod_endpoint_recovery_max_consecutive_attempts"),
        SettingId::from("yt_dlp.vod_endpoint_recovery_initial_backoff_ms"),
        SettingId::from("yt_dlp.vod_endpoint_recovery_max_backoff_ms"),
        SettingId::from("yt_dlp.vod_endpoint_recovery_stable_reset_ms"),
    ];
    assert_eq!(
        setting_ids_from_diff(&recovery_diff),
        expected_recovery_settings
    );

    let recovery_plan =
        runtime_route_plan_from_diff(&registry, &previous, &recovery_policy, &recovery_diff)
            .expect("VOD recovery route plan builds");
    let RuntimeCommittedUpdate::MediaService(media_update) =
        &recovery_plan.committed_routes[0].update
    else {
        panic!("VOD recovery должен создавать MediaService payload");
    };
    assert_eq!(media_update.network, previous.network);
    assert_eq!(media_update.yt_dlp, recovery_policy.yt_dlp);

    assert!(recovery_plan.preview_routes.is_empty());
    assert_eq!(
        recovery_plan.committed_routes,
        vec![RuntimeCommittedRoute {
            route: AppRuntimeRoute::MediaService,
            source_routes: vec![SettingRouteId::from(YT_DLP_ROUTE_ID)],
            // Exact vector закрепляет и полный набор, и registry-stable порядок ids.
            affected_settings: expected_recovery_settings.clone(),
            groups: vec![AppRuntimeRouteGroupUpdate {
                group: AppRuntimeRouteGroup::MediaYtDlp,
                affected_settings: expected_recovery_settings,
            }],
            update: RuntimeCommittedUpdate::MediaService(MediaServiceRuntimeSettingsUpdate {
                network: recovery_policy.network.clone(),
                yt_dlp: recovery_policy.yt_dlp.clone(),
            }),
        }]
    );
}
