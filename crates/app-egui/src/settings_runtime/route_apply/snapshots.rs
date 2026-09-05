//! Immutable committed snapshots и чистые route projections.
//!
//! Runtime preflight, owner mutation, rollback и finalize остаются в parent executor-е.

use super::*;

/// Строит успешный report для tests, где нет real worker-а.
pub(in crate::settings_runtime) fn simulated_player_runtime_report(
    update: PlayerRuntimeSettingsUpdate,
) -> PlayerRuntimeApplyReport {
    let mut report = PlayerRuntimeApplyReport::empty();

    if let Some(tick_update) = update.tick_config {
        report.push(simulated_player_group(
            PlayerRuntimeApplyGroup::TickConfig,
            tick_update.affected_settings,
            "player tick config accepted by test host",
        ));
    }
    if let Some(default_volume_update) = update.default_volume {
        report.push(simulated_player_group(
            PlayerRuntimeApplyGroup::DefaultVolume,
            default_volume_update.affected_settings,
            "default volume accepted by test host",
        ));
    }
    if let Some(decoder_thread_update) = update.decoder_thread_config {
        report.push(simulated_player_group(
            PlayerRuntimeApplyGroup::DecoderThreadConfig,
            decoder_thread_update.affected_settings,
            "decoder thread config accepted by test host",
        ));
    }
    if let Some(frame_server_policy_update) = update.frame_server_policy {
        report.push(simulated_player_group(
            PlayerRuntimeApplyGroup::FrameServerPolicy,
            frame_server_policy_update.affected_settings,
            "frame-server policy accepted by test host",
        ));
    }
    if report.groups.is_empty() {
        report.push(PlayerRuntimeApplyGroupReport::accepted(
            PlayerRuntimeApplyGroup::Request,
            std::iter::empty(),
            PlayerRuntimeAcceptedChange::Unchanged,
            "no player-core settings in update",
        ));
    }

    report
}

/// Создаёт accepted group report для fallback/test host-а.
fn simulated_player_group(
    group: PlayerRuntimeApplyGroup,
    affected_settings: Vec<player_core::PlayerRuntimeSettingId>,
    message: &'static str,
) -> PlayerRuntimeApplyGroupReport {
    PlayerRuntimeApplyGroupReport::accepted(
        group,
        affected_settings,
        PlayerRuntimeAcceptedChange::Applied,
        message,
    )
}

/// Выбирает mechanism по самому тяжёлому player operation в route.
pub(super) fn player_apply_mechanism(update: &PlayerCommittedSettingsUpdate) -> ApplyMechanism {
    if update.player_core.decoder_thread_config.is_some()
        || update.player_core.video_backend.is_some()
        || update.media_pipeline.is_some()
    {
        ApplyMechanism::PipelineRebuild
    } else if update.player_core.tick_config.is_some()
        || update.player_core.default_volume.is_some()
        || update.player_core.audio_output_recreate.is_some()
        || update.audio_output_device_id.is_some()
    {
        ApplyMechanism::WorkerReconfigure
    } else {
        ApplyMechanism::InPlace
    }
}

/// Преобразует worker report в route-level result без потери failure messages.
pub(super) fn player_runtime_report_result(
    report: &PlayerRuntimeApplyReport,
) -> AppRouteApplyResult {
    combine_player_in_place_results(report.groups.iter().map(player_runtime_group_report_result))
}

/// Преобразует одну player group в app route result.
fn player_runtime_group_report_result(
    report: &PlayerRuntimeApplyGroupReport,
) -> AppRouteApplyResult {
    match report.outcome {
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied) => {
            AppRouteApplyResult::Applied
        }
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Unchanged) => {
            AppRouteApplyResult::Noop
        }
        PlayerRuntimeApplyOutcome::RuntimeBusy(activity) => AppRouteApplyResult::RuntimeBusy {
            activity: match activity {
                PlayerRuntimeBoundaryActivity::Seek => SettingsBoundaryActivity::Seek,
                PlayerRuntimeBoundaryActivity::Scrub => SettingsBoundaryActivity::Scrub,
                PlayerRuntimeBoundaryActivity::PipelineLifecycle => {
                    SettingsBoundaryActivity::PipelineLifecycle
                }
            },
        },
        PlayerRuntimeApplyOutcome::Unsupported
        | PlayerRuntimeApplyOutcome::AbsentResource
        | PlayerRuntimeApplyOutcome::Invalid
        | PlayerRuntimeApplyOutcome::Fatal
        | PlayerRuntimeApplyOutcome::ApplyAndRollbackFailed => AppRouteApplyResult::Failed {
            message: format!("{:?}: {}", report.group, report.message),
        },
    }
}

/// Форматирует request/reply error без silent collapse.
pub(super) fn player_runtime_error_message(error: PlayerRuntimeApplyError) -> String {
    format!("player runtime apply failed: {error}")
}

/// Собирает результат independent in-place player updates без потери error details.
pub(super) fn combine_player_in_place_results(
    results: impl IntoIterator<Item = AppRouteApplyResult>,
) -> AppRouteApplyResult {
    let mut applied = false;
    let mut failures = Vec::new();
    let mut runtime_busy = None;

    for result in results {
        match result {
            AppRouteApplyResult::Applied | AppRouteApplyResult::PreviewPromoted => {
                applied = true;
            }
            AppRouteApplyResult::Noop => {}
            AppRouteApplyResult::Failed { message }
            | AppRouteApplyResult::PartialFailure { message } => failures.push(message),
            AppRouteApplyResult::RendererRecreationFailed { failure } => {
                failures.push(format!("renderer recreation failed: {failure:?}"));
            }
            AppRouteApplyResult::Conflict { baseline, current } => failures.push(format!(
                "conflict: baseline {}, current {}",
                baseline.value(),
                current.value()
            )),
            AppRouteApplyResult::RuntimeBusy { activity } => {
                runtime_busy.get_or_insert(activity);
            }
        }
    }

    if let Some(activity) = runtime_busy
        && failures.is_empty()
        && !applied
    {
        return AppRouteApplyResult::RuntimeBusy { activity };
    }
    if let Some(activity) = runtime_busy {
        failures.push(format!("runtime boundary is busy ({activity:?})"));
    }

    if failures.is_empty() {
        if applied {
            AppRouteApplyResult::Applied
        } else {
            AppRouteApplyResult::Noop
        }
    } else if applied {
        AppRouteApplyResult::PartialFailure {
            message: failures.join("; "),
        }
    } else {
        AppRouteApplyResult::Failed {
            message: failures.join("; "),
        }
    }
}

/// Возвращает group-level player result без слияния разных owner semantics.
pub(super) fn player_group_result(
    group: &AppRuntimeRouteGroup,
    player_core_result: &AppRouteApplyResult,
    audio_output_device_result: &AppRouteApplyResult,
    route_result: &AppRouteApplyResult,
) -> AppRouteApplyResult {
    match group {
        AppRuntimeRouteGroup::PlayerDefaultVolume
        | AppRuntimeRouteGroup::PlayerTickConfig
        | AppRuntimeRouteGroup::PlayerDecoderThreadConfig
        | AppRuntimeRouteGroup::PlayerVideoBackend
        | AppRuntimeRouteGroup::PlayerDeferredBoundary => player_core_result.clone(),
        AppRuntimeRouteGroup::PlayerAudioOutputDevice => audio_output_device_result.clone(),
        _ => route_result.clone(),
    }
}

/// Snapshot renderer lifecycle настроек, которые нельзя применить как live preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderCommittedRuntimeSnapshot {
    /// Renderer profile selection.
    profile: RenderProfile,

    /// Legacy tone mapping config.
    tone_mapping: ToneMappingMode,

    /// Vulkan committed settings.
    vulkan: VulkanConfig,

    /// OpenGL ES committed settings.
    opengles: fastiplayer_config::OpenGlesConfig,
}

impl RenderCommittedRuntimeSnapshot {
    /// Создаёт snapshot из full config-а.
    pub(super) fn from_config(config: &AppConfig) -> Self {
        Self {
            profile: config.render.profile,
            tone_mapping: config.render.tone_mapping,
            vulkan: config.render.vulkan.clone(),
            opengles: config.render.opengles.clone(),
        }
    }

    /// Создаёт snapshot из committed route payload-а.
    pub(super) fn from_update(update: &RenderCommittedSettingsUpdate) -> Self {
        Self {
            profile: update.profile,
            tone_mapping: update.tone_mapping,
            vulkan: update.vulkan.clone(),
            opengles: update.opengles.clone(),
        }
    }

    /// Восстанавливает typed payload предыдущей конфигурации для compensating rollback-а.
    pub(super) fn to_update(&self) -> RenderCommittedSettingsUpdate {
        RenderCommittedSettingsUpdate {
            profile: self.profile,
            tone_mapping: self.tone_mapping,
            vulkan: self.vulkan.clone(),
            opengles: self.opengles.clone(),
        }
    }
}

/// Snapshot player policy настроек, отделённый от current playback controls.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::settings_runtime) struct PlayerRuntimeSnapshot {
    /// Default volume policy для будущих media.
    pub(in crate::settings_runtime) default_volume: f32,

    /// Stable audio output device id для будущих audio outputs.
    pub(super) audio_output_device_id: String,
}

impl PlayerRuntimeSnapshot {
    /// Создаёт snapshot из full config-а.
    pub(in crate::settings_runtime) fn from_config(config: &AppConfig) -> Self {
        Self {
            default_volume: config.audio.volume as f32,
            audio_output_device_id: config.audio.output_device.clone(),
        }
    }
}

/// Snapshot media/service settings без владения конкретными network jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::settings_runtime) struct MediaServiceRuntimeSnapshot {
    /// Network/source cache policy.
    network: NetworkConfig,

    /// Provider-neutral web-media policy.
    web_media: WebMediaConfig,

    /// YtDlp extractor process controls.
    yt_dlp: YtDlpConfig,
}

impl MediaServiceRuntimeSnapshot {
    /// Создаёт snapshot из full config-а.
    pub(in crate::settings_runtime) fn from_config(config: &AppConfig) -> Self {
        Self {
            network: config.network.clone(),
            web_media: config.web_media.clone(),
            yt_dlp: config.yt_dlp.clone(),
        }
    }

    /// Создаёт snapshot из committed route payload-а.
    pub(super) fn from_update(update: &MediaServiceRuntimeSettingsUpdate) -> Self {
        Self {
            network: update.network.clone(),
            web_media: update.web_media.clone(),
            yt_dlp: update.yt_dlp.clone(),
        }
    }
}
