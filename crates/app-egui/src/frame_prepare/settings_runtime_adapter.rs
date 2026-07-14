//! Runtime settings adapter, который связывает UI transaction с app/player/media owners.

use super::*;
use crate::settings_runtime::SettingsRuntimePreflightFailure;

/// Runtime adapter одного frame-а: render live preview + committed runtime owners.
pub(super) struct FrameSettingsRuntimeAdapter<'frame> {
    /// Native window target нужен только renderer lifecycle owner-у.
    window: Arc<Window>,

    /// App state владеет player worker и app-level media/source identity.
    app_state: &'frame mut AppState,

    /// Renderer владеет WGPU context и live render settings.
    renderer: &'frame mut Renderer,

    /// Process-lifetime coordinator проводит settings reinstall до exact terminal.
    playlist_runtime: &'frame mut crate::playlist_runtime::PlaylistRuntime,

    /// Shell-level serializer renderer/surface lifecycle operations.
    renderer_lifecycle: &'frame mut RendererLifecycleCoordinator,
}

impl<'frame> FrameSettingsRuntimeAdapter<'frame> {
    /// Создаёт короткоживущий adapter без передачи ownership UI layer-у.
    pub(super) fn new(
        window: Arc<Window>,
        app_state: &'frame mut AppState,
        renderer: &'frame mut Renderer,
        playlist_runtime: &'frame mut crate::playlist_runtime::PlaylistRuntime,
        renderer_lifecycle: &'frame mut RendererLifecycleCoordinator,
    ) -> Self {
        Self {
            window,
            app_state,
            renderer,
            playlist_runtime,
            renderer_lifecycle,
        }
    }
}

/// Полный config snapshot для staged active-media reopen-а.
struct ActiveMediaReconfigureConfig {
    network: rustiplayer_config::NetworkConfig,
    youtube: rustiplayer_config::YoutubeConfig,
    demux: rustiplayer_config::PlayerDemuxConfig,
    preferred_video_codec_order: Vec<rustiplayer_config::VideoCodec>,
    reselect_youtube_stream: bool,
    rebuild_local_source: bool,
}

/// Сохраняет различие между доказанным pre-barrier failure и mutation, требующей rollback.
fn media_reconfigure_install_failure(
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

impl FrameSettingsRuntimeAdapter<'_> {
    /// Перестраивает reconstructible source и завершает route только после exact Installed/restore.
    fn reconfigure_active_media(
        &mut self,
        config: ActiveMediaReconfigureConfig,
    ) -> AppRouteApplyResult {
        let Some(active_source) = self.app_state.active_media_source() else {
            return AppRouteApplyResult::Applied;
        };
        if matches!(&active_source, ActiveMediaSource::LocalFile(_)) && !config.rebuild_local_source
        {
            return AppRouteApplyResult::Applied;
        }

        match self.app_state.runtime_reconfigure_boundary_activity() {
            Ok(Some(activity)) => {
                return AppRouteApplyResult::RuntimeBusy {
                    activity: settings_boundary_activity_from_player(activity),
                };
            }
            Ok(None) => {}
            Err(PlayerRuntimeApplyError::Backpressure) => {
                return AppRouteApplyResult::RuntimeBusy {
                    activity: SettingsBoundaryActivity::PipelineLifecycle,
                };
            }
            Err(error) => {
                return AppRouteApplyResult::Failed {
                    message: format!("media reconfigure preflight failed: {error}"),
                };
            }
        }

        let playback_snapshot = self.app_state.refresh_player_snapshot();
        let desired_intent = crate::state::playback_intent_from_snapshot(&playback_snapshot);
        let preferred_runtime_codecs = config
            .preferred_video_codec_order
            .iter()
            .copied()
            .map(runtime_video_codec)
            .collect::<Vec<_>>();

        let prepared_result: Result<crate::state::PreparedSingleMediaOpen, String> =
            match active_source {
                ActiveMediaSource::LocalFile(path) => {
                    match crate::local_media::prepare_local_file(&path, &config.demux) {
                        Ok(prepared_media) => {
                            let prepared_media = prepared_media
                                .with_preferred_video_codecs(&preferred_runtime_codecs);
                            let source = ActiveMediaSource::LocalFile(path.clone());
                            Ok(crate::state::PreparedSingleMediaOpen::new(
                                prepared_media,
                                source,
                                crate::media_open::SafeMediaLabel::from_local_path(&path),
                            ))
                        }
                        Err(error) => Err(format!("local media rebuild failed: {error:#}")),
                    }
                }
                ActiveMediaSource::DirectMediaUrl(source_locator) => {
                    match resolve_direct_media_startup_media(
                        &source_locator,
                        &config.network,
                        &config.demux,
                    ) {
                        Ok(opened_media) => {
                            let source_label = opened_media.source_label().to_string();
                            let prepared_media = PreparedMedia::from_external_label(
                                source_label,
                                opened_media.into_demuxer(),
                            )
                            .with_preferred_video_codecs(&preferred_runtime_codecs);
                            let safe_label =
                                crate::media_open::SafeMediaLabel::from_service_safe_label(
                                    source_locator.safe_label(),
                                );
                            Ok(crate::state::PreparedSingleMediaOpen::new(
                                prepared_media,
                                ActiveMediaSource::DirectMediaUrl(source_locator),
                                safe_label,
                            ))
                        }
                        Err(error) => Err(format!("direct media rebuild failed: {error:#}")),
                    }
                }
                ActiveMediaSource::YouTubeUrl {
                    source_locator,
                    selected_stream_identity,
                } => {
                    if config.reselect_youtube_stream {
                        let system_capabilities =
                            probe_system_capabilities(self.renderer.render_capabilities());
                        match resolve_youtube_startup_media(
                            &source_locator,
                            &config.network,
                            &config.youtube,
                            &config.demux,
                            &config.preferred_video_codec_order,
                            &system_capabilities,
                        ) {
                            Ok(prepared) => {
                                let prepared_media = PreparedMedia::from_external_label(
                                    prepared.streaming_media.description,
                                    prepared.streaming_media.demuxer,
                                );
                                let safe_label =
                                    crate::media_open::SafeMediaLabel::from_service_safe_label(
                                        source_locator.safe_label(),
                                    );
                                Ok(crate::state::PreparedSingleMediaOpen::new(
                                    prepared_media,
                                    ActiveMediaSource::YouTubeUrl {
                                        source_locator,
                                        selected_stream_identity: prepared.selected_stream_identity,
                                    },
                                    safe_label,
                                ))
                            }
                            Err(error) => Err(format!("YouTube media rebuild failed: {error:#}")),
                        }
                    } else {
                        match service_youtube::open_seekable_vod_from_selected_identity_with_demux_config(
                            &source_locator,
                            &selected_stream_identity,
                            &config.network,
                            &config.youtube,
                            &config.demux,
                        ) {
                            Ok(streaming_media) => {
                                let prepared_media = PreparedMedia::from_external_label(
                                    streaming_media.description,
                                    streaming_media.demuxer,
                                );
                                let safe_label =
                                    crate::media_open::SafeMediaLabel::from_service_safe_label(
                                        source_locator.safe_label(),
                                    );
                                Ok(crate::state::PreparedSingleMediaOpen::new(
                                    prepared_media,
                                    ActiveMediaSource::YouTubeUrl {
                                        source_locator,
                                        selected_stream_identity,
                                    },
                                    safe_label,
                                ))
                            }
                            Err(error) => Err(format!(
                                "selected YouTube media rebuild failed without changing stream: {error}"
                            )),
                        }
                    }
                }
            };

        let prepared_input = match prepared_result {
            Ok(prepared_input) => prepared_input,
            Err(message) => return AppRouteApplyResult::Failed { message },
        };
        let installed = match self.app_state.install_prepared_media_strong(
            self.playlist_runtime,
            self.renderer,
            prepared_input,
            desired_intent,
        ) {
            Ok(installed) => installed,
            Err(error) => {
                return media_reconfigure_install_failure(error);
            }
        };
        self.app_state
            .record_installed_media_source(installed.source.clone());
        if let Err(message) = self
            .app_state
            .restore_playback_after_media_reconfigure(&playback_snapshot, &installed)
        {
            return AppRouteApplyResult::PartialFailure { message };
        }
        AppRouteApplyResult::Applied
    }
}

impl RenderLiveSettingsAdapter for FrameSettingsRuntimeAdapter<'_> {
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.renderer.preview_live_settings(update)
    }

    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.renderer.commit_live_settings(settings)
    }

    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.renderer.rollback_live_settings(baseline)
    }
}

impl SettingsRuntimeReconfigureHost for FrameSettingsRuntimeAdapter<'_> {
    fn preflight_settings_transaction(
        &mut self,
        routes: &[rustiplayer_settings::RuntimeCommittedRoute],
    ) -> Result<(), SettingsRuntimePreflightFailure> {
        let touches_renderer = routes
            .iter()
            .any(|route| route.route == rustiplayer_settings::AppRuntimeRoute::RenderCommitted);
        if touches_renderer
            && let Some(activity) = self.renderer_lifecycle.settings_recreation_activity()
        {
            return Err(SettingsRuntimePreflightFailure {
                route: rustiplayer_settings::AppRuntimeRoute::RenderCommitted,
                result: AppRouteApplyResult::RuntimeBusy { activity },
            });
        }

        let touches_player_boundary = routes.iter().any(|route| {
            matches!(
                route.route,
                rustiplayer_settings::AppRuntimeRoute::Player
                    | rustiplayer_settings::AppRuntimeRoute::MediaService
                    | rustiplayer_settings::AppRuntimeRoute::FrameServer
            )
        });
        if !touches_player_boundary {
            return Ok(());
        }

        match self.app_state.runtime_reconfigure_boundary_activity() {
            Ok(Some(activity)) => Err(SettingsRuntimePreflightFailure {
                route: first_player_route(routes),
                result: AppRouteApplyResult::RuntimeBusy {
                    activity: settings_boundary_activity_from_player(activity),
                },
            }),
            Ok(None) => Ok(()),
            Err(PlayerRuntimeApplyError::Backpressure) => Err(SettingsRuntimePreflightFailure {
                route: first_player_route(routes),
                result: AppRouteApplyResult::RuntimeBusy {
                    activity: SettingsBoundaryActivity::PipelineLifecycle,
                },
            }),
            Err(error) => Err(SettingsRuntimePreflightFailure {
                route: first_player_route(routes),
                result: AppRouteApplyResult::Failed {
                    message: format!("settings transaction preflight failed: {error}"),
                },
            }),
        }
    }

    fn recreate_renderer(
        &mut self,
        previous: &RenderCommittedSettingsUpdate,
        next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult {
        let mut lifecycle =
            LiveRendererRecreation::new(self.window.clone(), self.renderer, self.app_state);
        self.renderer_lifecycle
            .recreate(&mut lifecycle, previous, next)
    }

    fn sync_committed_config_snapshot(&mut self, snapshot: CommittedConfigSnapshot) {
        self.app_state.sync_committed_config_snapshot(snapshot);
    }

    fn apply_player_runtime_settings(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
    ) -> PlayerRuntimeApplyResult {
        let mut report = PlayerRuntimeApplyReport::empty();
        let player_update = &update.player_core;
        let mut remaining_player_update = player_update.clone();

        if player_update.decoder_thread_config.is_some() || player_update.video_backend.is_some() {
            let backend_preference = player_update.video_backend.as_ref().map_or_else(
                || self.app_state.video_backend_preference(),
                |backend_update| match backend_update.preference {
                    player_core::PlayerRuntimeVideoBackendPreference::Auto => {
                        rustiplayer_config::VideoBackendPreference::Auto
                    }
                    player_core::PlayerRuntimeVideoBackendPreference::Hardware => {
                        rustiplayer_config::VideoBackendPreference::Hardware
                    }
                    player_core::PlayerRuntimeVideoBackendPreference::Software => {
                        rustiplayer_config::VideoBackendPreference::Software
                    }
                },
            );
            let decoder_thread_config = player_update.decoder_thread_config.as_ref().map_or_else(
                || self.app_state.current_decoder_thread_config(),
                |decoder_update| decoder_update.decoder_thread_config,
            );
            let stream_requirement = self.app_state.active_video_stream_requirement().cloned();
            if let Err(error) = self.app_state.rebuild_video_pipeline_with_decoder_config(
                VideoPipelineRebuildRequest {
                    backend_preference,
                    install_intent:
                        player_core::PlayerVideoBackendInstallIntent::SettingsReconfigure,
                    decoder_thread_config,
                    stream_requirement: stream_requirement.as_ref(),
                    instance: self.renderer.instance(),
                    adapter: self.renderer.adapter(),
                    device: self.renderer.device(),
                    queue: self.renderer.queue(),
                },
            ) {
                return Ok(player_pipeline_rebuild_failure_report(player_update, error));
            }

            if let Some(decoder_update) = &player_update.decoder_thread_config {
                report.push(PlayerRuntimeApplyGroupReport::accepted(
                    PlayerRuntimeApplyGroup::DecoderThreadConfig,
                    decoder_update.affected_settings.clone(),
                    player_core::PlayerRuntimeAcceptedChange::Applied,
                    "decoder config committed with controlled backend rebuild",
                ));
            }
            if let Some(backend_update) = &player_update.video_backend {
                report.push(PlayerRuntimeApplyGroupReport::accepted(
                    PlayerRuntimeApplyGroup::VideoBackend,
                    backend_update.affected_settings.clone(),
                    player_core::PlayerRuntimeAcceptedChange::Applied,
                    "video backend policy committed with controlled pipeline rebuild",
                ));
            }
            remaining_player_update.decoder_thread_config = None;
            remaining_player_update.video_backend = None;
        }

        if let Some(media_update) = &update.media_pipeline {
            let mut app_config = self.app_state.committed_app_config();
            app_config.player.demux = media_update.demux;
            app_config.player.preferred_video_codec_order =
                media_update.preferred_video_codec_order.clone();
            let reselect_youtube_stream = media_update
                .affected_settings
                .iter()
                .any(|setting_id| setting_id.as_str() == "player.preferred_video_codec_order");
            let media_result = self.reconfigure_active_media(ActiveMediaReconfigureConfig {
                network: app_config.network,
                youtube: app_config.youtube,
                demux: app_config.player.demux,
                preferred_video_codec_order: app_config.player.preferred_video_codec_order,
                reselect_youtube_stream,
                rebuild_local_source: true,
            });
            let affected_settings =
                player_runtime_ids_from_setting_ids(&media_update.affected_settings);
            match media_result {
                AppRouteApplyResult::Applied | AppRouteApplyResult::Noop => {
                    report.push(PlayerRuntimeApplyGroupReport::accepted(
                        PlayerRuntimeApplyGroup::MediaPipeline,
                        affected_settings,
                        player_core::PlayerRuntimeAcceptedChange::Applied,
                        "active media pipeline rebuilt with committed demux/codec policy",
                    ));
                }
                AppRouteApplyResult::RuntimeBusy { activity } => {
                    report.push(PlayerRuntimeApplyGroupReport::runtime_busy(
                        PlayerRuntimeApplyGroup::MediaPipeline,
                        affected_settings,
                        player_activity_from_settings(activity),
                        "active media pipeline boundary is busy",
                    ));
                    return Ok(report);
                }
                AppRouteApplyResult::Failed { message }
                | AppRouteApplyResult::PartialFailure { message } => {
                    report.push(PlayerRuntimeApplyGroupReport::fatal(
                        PlayerRuntimeApplyGroup::MediaPipeline,
                        affected_settings,
                        message,
                    ));
                    return Ok(report);
                }
                AppRouteApplyResult::RendererRecreationFailed { failure } => {
                    report.push(PlayerRuntimeApplyGroupReport::fatal(
                        PlayerRuntimeApplyGroup::MediaPipeline,
                        affected_settings,
                        format!("unexpected renderer recreation failure: {failure:?}"),
                    ));
                    return Ok(report);
                }
                AppRouteApplyResult::Conflict { .. } | AppRouteApplyResult::PreviewPromoted => {
                    report.push(PlayerRuntimeApplyGroupReport::fatal(
                        PlayerRuntimeApplyGroup::MediaPipeline,
                        affected_settings,
                        "unexpected media-pipeline route outcome",
                    ));
                    return Ok(report);
                }
            }
        }

        if !remaining_player_update.is_empty() {
            let worker_report = self
                .app_state
                .apply_player_runtime_settings(remaining_player_update)?;
            for group in worker_report.groups {
                report.push(group);
            }
        }

        if !update.event_policy_settings.is_empty() {
            report.push(PlayerRuntimeApplyGroupReport::accepted(
                PlayerRuntimeApplyGroup::EventPolicy,
                player_runtime_ids_from_setting_ids(&update.event_policy_settings),
                player_core::PlayerRuntimeAcceptedChange::Applied,
                "event-scoped player policy accepted for the next natural event",
            ));
        }

        Ok(report)
    }

    fn apply_media_service_runtime_settings(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
        affected_settings: &[SettingId],
    ) -> AppRouteApplyResult {
        if affected_settings
            .iter()
            .all(|setting_id| setting_id.as_str().starts_with("youtube."))
        {
            return self.app_state.apply_media_service_runtime_settings(update);
        }

        let mut app_config = self.app_state.committed_app_config();
        app_config.network = update.network.clone();
        app_config.youtube = update.youtube.clone();
        self.reconfigure_active_media(ActiveMediaReconfigureConfig {
            network: app_config.network,
            youtube: app_config.youtube,
            demux: app_config.player.demux,
            preferred_video_codec_order: app_config.player.preferred_video_codec_order,
            reselect_youtube_stream: false,
            rebuild_local_source: false,
        })
    }
}

/// Конвертирует project setting ids в typed player report ids.
fn player_runtime_ids_from_setting_ids(
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
const fn player_activity_from_settings(
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
const fn settings_boundary_activity_from_player(
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
fn first_player_route(
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
fn player_pipeline_rebuild_failure_report(
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
mod strong_completion_tests {
    use super::*;

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
