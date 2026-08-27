//! Чистая классификация runtime settings и typed projection ошибок владельцев.

use super::*;

/// Stable setting id, единственный YtDlp policy change с active-source reselection.
const PREFERRED_VIDEO_HEIGHT_SETTING_ID: &str = "yt_dlp.preferred_video_height";

/// Проверяет intent переоткрыть active YtDlp source с новым global preference.
pub(super) fn requires_yt_dlp_stream_reselection(affected_settings: &[SettingId]) -> bool {
    affected_settings
        .iter()
        .any(|setting_id| setting_id.as_str() == PREFERRED_VIDEO_HEIGHT_SETTING_ID)
}

/// Проверяет, содержит ли route изменение transport/network policy.
pub(super) fn requires_remote_source_rebuild(affected_settings: &[SettingId]) -> bool {
    affected_settings
        .iter()
        .any(|setting_id| setting_id.as_str().starts_with("network."))
}

/// Сохраняет различие между доказанным pre-barrier failure и mutation, требующей rollback.
pub(super) fn media_reconfigure_install_failure(
    error: crate::state::StrongMediaOpenError,
) -> AppRouteApplyResult {
    let message =
        format!("media rebuild failed while waiting for strong install completion: {error}");
    if error.may_have_crossed_install_barrier() {
        AppRouteApplyResult::PartialFailure { message }
    } else {
        AppRouteApplyResult::Failed { message }
    }
}

/// Конвертирует project setting ids в typed player report ids.
pub(super) fn player_runtime_ids_from_setting_ids(
    setting_ids: &[SettingId],
) -> Vec<player_core::PlayerRuntimeSettingId> {
    setting_ids
        .iter()
        .filter_map(|setting_id| match setting_id.as_str() {
            "player.start_paused" => Some(player_core::PlayerRuntimeSettingId::PlayerStartPaused),
            "player.resume_last_position" => {
                Some(player_core::PlayerRuntimeSettingId::PlayerResumeLastPosition)
            }
            "player.seek.paused_commit_behavior" => {
                Some(player_core::PlayerRuntimeSettingId::PlayerSeekPausedCommitBehavior)
            }
            "player.seek.hotkey_small_step_secs" => {
                Some(player_core::PlayerRuntimeSettingId::PlayerSeekHotkeySmallStepSecs)
            }
            "player.seek.hotkey_large_step_secs" => {
                Some(player_core::PlayerRuntimeSettingId::PlayerSeekHotkeyLargeStepSecs)
            }
            "player.preferred_video_codec_order" => {
                Some(player_core::PlayerRuntimeSettingId::PlayerPreferredVideoCodecOrder)
            }
            "player.demux.max_consecutive_corrupted_packets" => {
                Some(player_core::PlayerRuntimeSettingId::PlayerDemuxMaxConsecutiveCorruptedPackets)
            }
            _ => None,
        })
        .collect()
}

/// Возвращает neutral player activity для worker-level report-а.
pub(super) const fn player_activity_from_settings(
    activity: SettingsBoundaryActivity,
) -> player_core::PlayerRuntimeBoundaryActivity {
    match activity {
        SettingsBoundaryActivity::Seek => player_core::PlayerRuntimeBoundaryActivity::Seek,
        SettingsBoundaryActivity::Scrub => player_core::PlayerRuntimeBoundaryActivity::Scrub,
        SettingsBoundaryActivity::SettingsTransaction
        | SettingsBoundaryActivity::PipelineLifecycle
        | SettingsBoundaryActivity::RendererLifecycle => {
            player_core::PlayerRuntimeBoundaryActivity::PipelineLifecycle
        }
    }
}

/// Сопоставляет neutral player busy activity с project settings contract.
pub(super) const fn settings_boundary_activity_from_player(
    activity: player_core::PlayerRuntimeBoundaryActivity,
) -> SettingsBoundaryActivity {
    match activity {
        player_core::PlayerRuntimeBoundaryActivity::Seek => SettingsBoundaryActivity::Seek,
        player_core::PlayerRuntimeBoundaryActivity::Scrub => SettingsBoundaryActivity::Scrub,
        player_core::PlayerRuntimeBoundaryActivity::PipelineLifecycle => {
            SettingsBoundaryActivity::PipelineLifecycle
        }
    }
}

/// Находит первый затронутый player/media owner для точного preflight report-а.
pub(super) fn first_player_route(
    routes: &[rustiplayer_settings::RuntimeCommittedRoute],
) -> rustiplayer_settings::AppRuntimeRoute {
    routes
        .iter()
        .map(|route| route.route)
        .find(|route| {
            matches!(
                route,
                rustiplayer_settings::AppRuntimeRoute::Player
                    | rustiplayer_settings::AppRuntimeRoute::MediaService
                    | rustiplayer_settings::AppRuntimeRoute::FrameServer
            )
        })
        .unwrap_or(rustiplayer_settings::AppRuntimeRoute::Player)
}

/// Строит typed player report, если app-owned pipeline rebuild не стартовал.
pub(super) fn player_pipeline_rebuild_failure_report(
    update: &PlayerRuntimeSettingsUpdate,
    error: VideoPipelineRebuildError,
) -> PlayerRuntimeApplyReport {
    let mut report = PlayerRuntimeApplyReport::empty();
    let message = error.to_string();
    let runtime_busy = match &error {
        VideoPipelineRebuildError::Worker(PlayerRuntimeApplyError::RuntimeBusy(activity)) => {
            Some(*activity)
        }
        _ => None,
    };
    let apply_and_rollback_failed = matches!(
        error,
        VideoPipelineRebuildError::Worker(PlayerRuntimeApplyError::ApplyAndRollbackFailed { .. })
    );
    let group_report = |group, affected_settings, group_message: String| {
        if let Some(activity) = runtime_busy {
            PlayerRuntimeApplyGroupReport::runtime_busy(
                group,
                affected_settings,
                activity,
                group_message,
            )
        } else if apply_and_rollback_failed {
            PlayerRuntimeApplyGroupReport::apply_and_rollback_failed(
                group,
                affected_settings,
                group_message,
            )
        } else {
            PlayerRuntimeApplyGroupReport::fatal(group, affected_settings, group_message)
        }
    };
    if let Some(decoder_update) = &update.decoder_thread_config {
        report.push(group_report(
            PlayerRuntimeApplyGroup::DecoderThreadConfig,
            decoder_update.affected_settings.clone(),
            message.clone(),
        ));
    }
    if let Some(backend_update) = &update.video_backend {
        report.push(group_report(
            PlayerRuntimeApplyGroup::VideoBackend,
            backend_update.affected_settings.clone(),
            message,
        ));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_global_preferred_height_requests_yt_dlp_reselection() {
        assert!(requires_yt_dlp_stream_reselection(&[SettingId::from(
            PREFERRED_VIDEO_HEIGHT_SETTING_ID,
        )]));
        assert!(!requires_yt_dlp_stream_reselection(&[
            SettingId::from("yt_dlp.hdr_selection"),
            SettingId::from("yt_dlp.item_video_height_override"),
        ]));
        assert!(!requires_remote_source_rebuild(&[SettingId::from(
            PREFERRED_VIDEO_HEIGHT_SETTING_ID,
        )]));
        assert!(requires_remote_source_rebuild(&[
            SettingId::from(PREFERRED_VIDEO_HEIGHT_SETTING_ID),
            SettingId::from("network.read_ahead_mb"),
        ]));
    }

    #[test]
    fn install_barrier_failure_enters_settings_compensation() {
        let result =
            media_reconfigure_install_failure(crate::state::StrongMediaOpenError::MissingTerminal);

        assert!(matches!(result, AppRouteApplyResult::PartialFailure { .. }));
    }

    #[test]
    fn proven_pre_barrier_failure_does_not_request_settings_compensation() {
        let result = media_reconfigure_install_failure(crate::state::StrongMediaOpenError::Start(
            crate::media_open::MediaOpenStartError::Busy,
        ));

        assert!(matches!(result, AppRouteApplyResult::Failed { .. }));
    }
}
