use super::*;

mod snapshots;

pub(super) use snapshots::{
    MediaServiceRuntimeSnapshot, PlayerRuntimeSnapshot, simulated_player_runtime_report,
};
use snapshots::{
    RenderCommittedRuntimeSnapshot, combine_player_in_place_results, player_apply_mechanism,
    player_group_result, player_runtime_error_message, player_runtime_report_result,
};

/// Typed read-only preflight rejection tied to the owner that is busy.
pub(crate) struct SettingsRuntimePreflightFailure {
    pub(crate) route: rustiplayer_settings::AppRuntimeRoute,
    pub(crate) result: AppRouteApplyResult,
}

/// Boundary для runtime owners, которые живут снаружи settings controller-а.
///
/// Visual settings UI не знает про worker, source jobs или concrete backend.
/// Этот trait передаётся только из app composition/frame слоя, где эти owners
/// реально доступны и где можно сохранить порядок команд.
pub(crate) trait SettingsRuntimeReconfigureHost {
    /// Проверяет все затронутые lifecycle boundaries до первой owner mutation.
    fn preflight_settings_transaction(
        &mut self,
        _routes: &[RuntimeCommittedRoute],
    ) -> Result<(), SettingsRuntimePreflightFailure> {
        Ok(())
    }

    /// Синхронизирует внешний snapshot только после persistence и finalize.
    fn sync_committed_config_snapshot(&mut self, _snapshot: CommittedConfigSnapshot) {}

    /// Возвращает live sidebar geometry к committed значению после persistence failure.
    fn restore_sidebar_width(&mut self, _width_points: crate::ui::sidebar::SidebarWidthPoints) {}

    /// Завершает staged irreversible work после successful persistence.
    fn finalize_settings_transaction(&mut self) {}

    /// Обратимо stage-ит playlist policy у process-lifetime owner-а.
    fn apply_playlist_runtime_settings(
        &mut self,
        _update: &PlaylistRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult {
        AppRouteApplyResult::Noop
    }

    /// Компенсирует staged playlist policy до finalize.
    fn rollback_playlist_runtime_settings(&mut self) -> AppRouteApplyResult {
        AppRouteApplyResult::Noop
    }

    /// Транзакционно пересоздаёт renderer/surface path у shell lifecycle owner-а.
    fn recreate_renderer(
        &mut self,
        previous: &RenderCommittedSettingsUpdate,
        next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult;

    /// Применяет typed player update через owner-level worker boundary.
    fn apply_player_runtime_settings(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
    ) -> PlayerRuntimeApplyResult;

    /// Применяет media/source policy и при необходимости запускает controlled rebuild.
    fn apply_media_service_runtime_settings(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
        affected_settings: &[SettingId],
        target_policy: SettingsRouteTargetPolicy,
    ) -> AppRouteApplyResult;
}

/// Fallback host для unit tests и UI-only paths без active `AppState`.
pub(super) struct NoopSettingsRuntimeReconfigureHost;

impl SettingsRuntimeReconfigureHost for NoopSettingsRuntimeReconfigureHost {
    fn recreate_renderer(
        &mut self,
        _previous: &RenderCommittedSettingsUpdate,
        _next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult {
        AppRouteApplyResult::Applied
    }

    fn apply_player_runtime_settings(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
        _target_policy: SettingsRouteTargetPolicy,
    ) -> PlayerRuntimeApplyResult {
        let mut report = if update.player_core.is_empty() {
            PlayerRuntimeApplyReport::empty()
        } else {
            simulated_player_runtime_report(update.player_core.clone())
        };
        if update.media_pipeline.is_some() {
            report.push(PlayerRuntimeApplyGroupReport::accepted(
                PlayerRuntimeApplyGroup::MediaPipeline,
                std::iter::empty(),
                PlayerRuntimeAcceptedChange::Applied,
                "simulated app-owned media pipeline rebuild",
            ));
        }
        if !update.event_policy_settings.is_empty() {
            report.push(PlayerRuntimeApplyGroupReport::accepted(
                PlayerRuntimeApplyGroup::EventPolicy,
                std::iter::empty(),
                PlayerRuntimeAcceptedChange::Applied,
                "simulated event policy update",
            ));
        }
        Ok(report)
    }

    fn apply_media_service_runtime_settings(
        &mut self,
        _update: &MediaServiceRuntimeSettingsUpdate,
        _affected_settings: &[SettingId],
        _target_policy: SettingsRouteTargetPolicy,
    ) -> AppRouteApplyResult {
        AppRouteApplyResult::Applied
    }
}

/// Adapter для старых call sites, которым нужен только render live adapter.
#[cfg(test)]
pub(super) struct RenderOnlySettingsRuntimeAdapter<'adapter, A> {
    /// Реальный renderer-neutral adapter.
    pub(super) render_adapter: &'adapter mut A,
}

#[cfg(test)]
impl<A> RenderLiveSettingsAdapter for RenderOnlySettingsRuntimeAdapter<'_, A>
where
    A: RenderLiveSettingsAdapter,
{
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.render_adapter.preview_live_settings(update)
    }

    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.render_adapter.commit_live_settings(settings)
    }

    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.render_adapter.rollback_live_settings(baseline)
    }
}

#[cfg(test)]
impl<A> SettingsRuntimeReconfigureHost for RenderOnlySettingsRuntimeAdapter<'_, A>
where
    A: RenderLiveSettingsAdapter,
{
    fn recreate_renderer(
        &mut self,
        previous: &RenderCommittedSettingsUpdate,
        next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult {
        let mut host = NoopSettingsRuntimeReconfigureHost;
        host.recreate_renderer(previous, next)
    }

    fn apply_player_runtime_settings(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
    ) -> PlayerRuntimeApplyResult {
        let mut host = NoopSettingsRuntimeReconfigureHost;
        host.apply_player_runtime_settings(update, target_policy)
    }

    fn apply_media_service_runtime_settings(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
        affected_settings: &[SettingId],
        target_policy: SettingsRouteTargetPolicy,
    ) -> AppRouteApplyResult {
        let mut host = NoopSettingsRuntimeReconfigureHost;
        host.apply_media_service_runtime_settings(update, affected_settings, target_policy)
    }
}

/// Concrete route appliers/status snapshots for settings runtime.
pub(super) struct SettingsRuntimeRouteAppliers {
    /// Последний UI runtime snapshot.
    ui: UiConfig,

    /// Последний live render snapshot, известный settings runtime-у.
    pub(super) render_live: RenderLiveSettings,

    /// Последний committed renderer lifecycle snapshot.
    render_committed: RenderCommittedRuntimeSnapshot,

    /// Последний player settings snapshot, не смешанный с current playback controls.
    pub(super) player: PlayerRuntimeSnapshot,

    /// Shared audio output device selection owner из concrete audio crate.
    pub(super) audio_output_device_controller: audio::AudioOutputDeviceController,

    /// Последний media/service policy snapshot.
    pub(super) media_service: MediaServiceRuntimeSnapshot,

    /// Последний committed Frame Server snapshot после успешного live policy apply.
    pub(super) frame_server: rustiplayer_config::FrameServerConfig,
}

impl SettingsRuntimeRouteAppliers {
    /// Инициализирует route snapshots из startup committed config-а.
    pub(super) fn from_config(config: &AppConfig) -> SettingsResult<Self> {
        Ok(Self {
            ui: config.ui.clone(),
            render_live: render_live_settings_from_config(config)?,
            render_committed: RenderCommittedRuntimeSnapshot::from_config(config),
            player: PlayerRuntimeSnapshot::from_config(config),
            audio_output_device_controller: audio::AudioOutputDeviceController::new(
                config.audio.output_device.clone(),
            ),
            media_service: MediaServiceRuntimeSnapshot::from_config(config),
            frame_server: config.frame_server.clone(),
        })
    }

    /// Единый helper для route report-а с одинаковым результатом по всем groups.
    pub(super) fn route_report(
        route: RuntimeCommittedRoute,
        result: AppRouteApplyResult,
        mechanism: ApplyMechanism,
    ) -> AppRouteApplyReport {
        AppRouteApplyReport {
            route: route.route,
            source_routes: route.source_routes,
            result: result.clone(),
            mechanism,
            affected_settings: route.affected_settings,
            groups: group_reports(route.groups, result),
        }
    }
}

impl AppRuntimeRouteApplier for SettingsRuntimeRouteAppliers {
    fn preflight_committed_routes(
        &mut self,
        _routes: &[RuntimeCommittedRoute],
    ) -> SettingsResult<Option<AppRouteApplyReport>> {
        Ok(None)
    }

    fn apply_committed_route(
        &mut self,
        route: RuntimeCommittedRoute,
    ) -> SettingsResult<AppRouteApplyReport> {
        if let RuntimeCommittedUpdate::RenderPreview(update) = route.update.clone() {
            self.render_live = update.live_settings.settings.clone();
            return Ok(Self::route_report(
                route,
                AppRouteApplyResult::PreviewPromoted,
                ApplyMechanism::PreviewPromoted,
            ));
        }

        let mut reconfigure_host = NoopSettingsRuntimeReconfigureHost;
        self.apply_committed_route_with_reconfigure_host(
            route,
            SettingsRouteTargetPolicy::ExternalOwnersUnavailable,
            &mut reconfigure_host,
        )
    }

    fn rollback_committed_route(
        &mut self,
        route: RuntimeCommittedRoute,
    ) -> SettingsResult<AppRouteApplyReport> {
        self.apply_committed_route(route)
    }

    fn finalize_committed_routes(&mut self) {}
}

impl SettingsRuntimeRouteAppliers {
    /// Применяет committed route с доступом к renderer-neutral live adapter.
    pub(super) fn apply_committed_route_with_render_adapter<A>(
        &mut self,
        route: RuntimeCommittedRoute,
        target_policy: SettingsRouteTargetPolicy,
        runtime_adapter: &mut A,
    ) -> SettingsResult<AppRouteApplyReport>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        match route.update.clone() {
            RuntimeCommittedUpdate::RenderPreview(update) => {
                let result = self
                    .commit_render_preview_update(&update.live_settings.settings, runtime_adapter);
                Ok(Self::route_report(
                    route,
                    result,
                    ApplyMechanism::PreviewPromoted,
                ))
            }
            _ => self.apply_committed_route_with_reconfigure_host(
                route,
                target_policy,
                runtime_adapter,
            ),
        }
    }

    /// Компенсирует committed route; preview использует rollback, а не повторный commit.
    pub(super) fn rollback_committed_route_with_render_adapter<A>(
        &mut self,
        route: RuntimeCommittedRoute,
        target_policy: SettingsRouteTargetPolicy,
        runtime_adapter: &mut A,
    ) -> SettingsResult<AppRouteApplyReport>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        match route.update.clone() {
            RuntimeCommittedUpdate::RenderPreview(update) => {
                let result = self.rollback_committed_render_preview_update(
                    &update.live_settings.settings,
                    runtime_adapter,
                );
                Ok(Self::route_report(
                    route,
                    result,
                    ApplyMechanism::PreviewPromoted,
                ))
            }
            RuntimeCommittedUpdate::Playlist(_) => {
                // Компенсация не повторяет apply: staged playlist owner сам хранит exact baseline.
                let result = runtime_adapter.rollback_playlist_runtime_settings();
                Ok(Self::route_report(route, result, ApplyMechanism::InPlace))
            }
            _ => self.apply_committed_route_with_reconfigure_host(
                route,
                target_policy,
                runtime_adapter,
            ),
        }
    }

    /// Применяет committed route с доступом к app/player/source owners.
    fn apply_committed_route_with_reconfigure_host(
        &mut self,
        route: RuntimeCommittedRoute,
        target_policy: SettingsRouteTargetPolicy,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> SettingsResult<AppRouteApplyReport> {
        match route.update.clone() {
            RuntimeCommittedUpdate::Ui(update) => {
                let result = self.apply_ui_update(&update);
                Ok(Self::route_report(route, result, ApplyMechanism::InPlace))
            }
            RuntimeCommittedUpdate::RenderCommitted(update) => {
                let result = self.apply_render_committed_update(&update, reconfigure_host);
                Ok(Self::route_report(
                    route,
                    result,
                    ApplyMechanism::RendererRecreate,
                ))
            }
            RuntimeCommittedUpdate::Player(update) => {
                Ok(self.apply_player_route(route, &update, target_policy, reconfigure_host))
            }
            RuntimeCommittedUpdate::MediaService(update) => {
                let policy_only = route
                    .affected_settings
                    .iter()
                    .all(|setting_id| setting_id.as_str().starts_with("yt_dlp."));
                let result = self.apply_media_service_update(
                    &update,
                    &route.affected_settings,
                    target_policy,
                    reconfigure_host,
                );
                Ok(Self::route_report(
                    route,
                    result,
                    if policy_only {
                        ApplyMechanism::InPlace
                    } else {
                        ApplyMechanism::PipelineRebuild
                    },
                ))
            }
            RuntimeCommittedUpdate::FrameServer(update) => {
                let result =
                    self.apply_frame_server_update(&update, target_policy, reconfigure_host);
                Ok(Self::route_report(
                    route,
                    result,
                    ApplyMechanism::WorkerReconfigure,
                ))
            }
            RuntimeCommittedUpdate::Playlist(update) => {
                let result = reconfigure_host.apply_playlist_runtime_settings(&update);
                Ok(Self::route_report(route, result, ApplyMechanism::InPlace))
            }
            RuntimeCommittedUpdate::RenderPreview(_update) => unreachable!(
                "render preview committed route requires render adapter promotion path"
            ),
        }
    }

    /// Применяет UI shell/settings-runtime snapshot.
    fn apply_ui_update(&mut self, update: &UiRuntimeSettingsUpdate) -> AppRouteApplyResult {
        if self.ui == update.ui {
            return AppRouteApplyResult::Noop;
        }

        self.ui = update.ui.clone();
        AppRouteApplyResult::Applied
    }

    /// Применяет Frame Server policy через player/app runtime boundaries.
    fn apply_frame_server_update(
        &mut self,
        update: &FrameServerRuntimeSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> AppRouteApplyResult {
        if self.frame_server == update.frame_server {
            return AppRouteApplyResult::Noop;
        }
        let local_snapshot_result = AppRouteApplyResult::Applied;
        let player_update = PlayerCommittedSettingsUpdate {
            player_core: update.player_core.clone(),
            audio_output_device_id: None,
            event_policy_settings: Vec::new(),
            resume_last_position: None,
            media_pipeline: None,
        };

        let player_result =
            match reconfigure_host.apply_player_runtime_settings(&player_update, target_policy) {
                Ok(report) => player_runtime_report_result(&report),
                Err(error) => AppRouteApplyResult::Failed {
                    message: player_runtime_error_message(error),
                },
            };
        if !player_result.is_success() {
            return player_result;
        }

        self.frame_server = update.frame_server.clone();
        combine_player_in_place_results([local_snapshot_result, player_result])
    }

    /// Применяет committed render lifecycle snapshot только после успешного recreation commit-а.
    fn apply_render_committed_update(
        &mut self,
        update: &RenderCommittedSettingsUpdate,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> AppRouteApplyResult {
        let next_snapshot = RenderCommittedRuntimeSnapshot::from_update(update);
        if self.render_committed == next_snapshot {
            return AppRouteApplyResult::Noop;
        }

        let previous_update = self.render_committed.to_update();
        let result = reconfigure_host.recreate_renderer(&previous_update, update);
        if result.is_success() {
            self.render_committed = next_snapshot;
        }
        result
    }

    fn apply_player_route(
        &mut self,
        route: RuntimeCommittedRoute,
        update: &PlayerCommittedSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> AppRouteApplyReport {
        let previous_audio_output_device_id = self.player.audio_output_device_id.clone();
        let requested_audio_device_change = update
            .audio_output_device_id
            .as_ref()
            .is_some_and(|next_device_id| next_device_id != &previous_audio_output_device_id);
        let mut audio_output_device_result = self.apply_player_audio_output_device(update);

        let player_core_result = if audio_output_device_result.is_success() {
            self.apply_player_core_update(update, target_policy, reconfigure_host)
        } else {
            AppRouteApplyResult::Failed {
                message: "player owner apply was not started because audio device policy failed"
                    .to_string(),
            }
        };

        if !player_core_result.is_success() && requested_audio_device_change {
            audio_output_device_result = match self
                .audio_output_device_controller
                .select_output_device(previous_audio_output_device_id.clone())
            {
                Ok(_) => AppRouteApplyResult::Failed {
                    message: "active audio output recreation failed; device policy was rolled back"
                        .to_string(),
                },
                Err(rollback_error) => AppRouteApplyResult::PartialFailure {
                    message: format!(
                        "active audio output recreation failed and device policy rollback failed: {rollback_error}"
                    ),
                },
            };
        }

        if player_core_result.is_success() && audio_output_device_result.is_success() {
            self.commit_player_default_volume_snapshot(update);
            if let Some(next_device_id) = &update.audio_output_device_id {
                self.player.audio_output_device_id = next_device_id.clone();
            }
        }

        let in_place_result = combine_player_in_place_results([
            player_core_result.clone(),
            audio_output_device_result.clone(),
        ]);
        let route_result = in_place_result;
        let mechanism = if route_result.is_success() {
            player_apply_mechanism(update)
        } else {
            ApplyMechanism::WorkerReconfigure
        };
        let groups = route
            .groups
            .into_iter()
            .map(|group| AppRouteGroupReport {
                result: player_group_result(
                    &group.group,
                    &player_core_result,
                    &audio_output_device_result,
                    &route_result,
                ),
                group: group.group,
                affected_settings: group.affected_settings,
            })
            .collect();

        AppRouteApplyReport {
            route: route.route,
            source_routes: route.source_routes,
            result: route_result,
            mechanism,
            affected_settings: route.affected_settings,
            groups,
        }
    }

    /// Применяет player-core update через request/reply worker boundary.
    fn apply_player_core_update(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> AppRouteApplyResult {
        if update.player_core.is_empty()
            && update.event_policy_settings.is_empty()
            && update.media_pipeline.is_none()
        {
            return AppRouteApplyResult::Noop;
        };

        match reconfigure_host.apply_player_runtime_settings(update, target_policy) {
            Ok(report) => player_runtime_report_result(&report),
            Err(error) => AppRouteApplyResult::Failed {
                message: player_runtime_error_message(error),
            },
        }
    }

    /// Обновляет локальный policy snapshot только после успешного worker apply.
    fn commit_player_default_volume_snapshot(&mut self, update: &PlayerCommittedSettingsUpdate) {
        let Some(default_volume_update) = &update.player_core.default_volume else {
            return;
        };
        let next_default_volume = default_volume_update.default_volume;
        if (self.player.default_volume - next_default_volume).abs() > f32::EPSILON {
            self.player.default_volume = next_default_volume;
        }
    }

    /// Применяет выбранный audio output device через owner API concrete audio crate.
    fn apply_player_audio_output_device(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
    ) -> AppRouteApplyResult {
        let Some(next_device_id) = &update.audio_output_device_id else {
            return AppRouteApplyResult::Noop;
        };
        if &self.player.audio_output_device_id == next_device_id {
            return AppRouteApplyResult::Noop;
        }

        match self
            .audio_output_device_controller
            .select_output_device(next_device_id.clone())
        {
            Ok(audio::AudioOutputDeviceSelectionChange::Applied) => AppRouteApplyResult::Applied,
            Ok(audio::AudioOutputDeviceSelectionChange::Noop) => AppRouteApplyResult::Noop,
            Err(error) => AppRouteApplyResult::Failed {
                message: error.to_string(),
            },
        }
    }

    /// Применяет media/service policy snapshot на уровне app settings runtime.
    fn apply_media_service_update(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
        affected_settings: &[SettingId],
        target_policy: SettingsRouteTargetPolicy,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> AppRouteApplyResult {
        let next_snapshot = MediaServiceRuntimeSnapshot::from_update(update);
        if self.media_service == next_snapshot {
            return AppRouteApplyResult::Noop;
        }

        let result = reconfigure_host.apply_media_service_runtime_settings(
            update,
            affected_settings,
            target_policy,
        );
        if !result.is_success() {
            return result;
        }

        self.media_service = next_snapshot;
        result
    }
}
