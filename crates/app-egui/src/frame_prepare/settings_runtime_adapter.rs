//! Runtime settings adapter, который связывает UI transaction с app/player/media owners.

#[path = "settings_runtime_adapter/reconfigure_projection.rs"]
mod reconfigure_projection;

use super::*;
use crate::settings_runtime::SettingsRuntimePreflightFailure;
use reconfigure_projection::{
    first_player_route, media_reconfigure_install_failure, player_activity_from_settings,
    player_pipeline_rebuild_failure_report, player_runtime_ids_from_setting_ids,
    requires_remote_source_rebuild, requires_web_media_stream_reselection,
    settings_boundary_activity_from_player,
};

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
    web_media: rustiplayer_config::WebMediaConfig,
    yt_dlp: rustiplayer_config::YtDlpConfig,
    demux: rustiplayer_config::PlayerDemuxConfig,
    preferred_video_codec_order: Vec<rustiplayer_config::VideoCodec>,
    video_backend_preference: rustiplayer_config::VideoBackendPreference,
    reselect_web_media_stream: bool,
    rebuild_remote_source: bool,
    rebuild_local_source: bool,
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
        let playback_window = active_source.playback_window();
        if matches!(
            active_source.physical_source(),
            ActiveMediaSource::LocalFile(_)
        ) && !config.rebuild_local_source
        {
            return AppRouteApplyResult::Applied;
        }
        let web_reconfigure_policy = crate::media_open::WebMediaSettingsReconfigurePolicy {
            direct_resource: if config.rebuild_remote_source {
                crate::media_open::DirectResourceSettingsAction::Rebuild
            } else {
                crate::media_open::DirectResourceSettingsAction::KeepInstalled
            },
            selection: if config.reselect_web_media_stream {
                crate::media_open::WebMediaSettingsSelectionPolicy::ReselectBestPlayable
            } else {
                crate::media_open::WebMediaSettingsSelectionPolicy::PreserveInstalled
            },
        };
        if active_source
            .web_intent()
            .is_some_and(|source| !source.requires_settings_reconfigure(web_reconfigure_policy))
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
            match active_source.into_physical_source() {
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
                ActiveMediaSource::Web(web_intent) => {
                    let system_capabilities =
                        probe_system_capabilities(self.renderer.render_capabilities());
                    let adaptive_settings = crate::media_open::WebMediaOpenSettings {
                        network_config: config.network.clone(),
                        web_media_config: config.web_media.clone(),
                        yt_dlp_config: config.yt_dlp.clone(),
                        demux_config: config.demux,
                        preferred_video_codec_order: config.preferred_video_codec_order.clone(),
                        system_capabilities: Box::new(system_capabilities),
                        audio_capabilities: self.app_state.audio_decode_capability_snapshot(),
                    };
                    let request = match web_intent.settings_reconfigure_request(
                        web_reconfigure_policy,
                        config.network.clone(),
                        config.demux,
                        adaptive_settings,
                    ) {
                        crate::media_open::WebMediaSettingsReconfigureDecision::NoChange => {
                            return AppRouteApplyResult::Applied;
                        }
                        crate::media_open::WebMediaSettingsReconfigureDecision::Reopen(request) => {
                            request
                        }
                    };
                    let safe_label = request.safe_label();
                    match crate::media_open::prepare_source_synchronously(
                        crate::media_open::MediaOpenSourceRequest::Web(request),
                    ) {
                        Ok(prepared_open) => {
                            let (prepared_media, descriptor) = prepared_open.into_parts();
                            let source = descriptor.active_source();
                            Ok(crate::state::PreparedSingleMediaOpen::new(
                                prepared_media
                                    .with_preferred_video_codecs(&preferred_runtime_codecs),
                                source,
                                safe_label,
                            )
                            .with_descriptor(descriptor))
                        }
                        Err(error) => Err(format!("web media rebuild failed: {error:?}")),
                    }
                }
                ActiveMediaSource::PlaybackWindow { .. } => {
                    unreachable!("into_physical_source removes playback-window wrappers")
                }
            };

        let prepared_input = match prepared_result {
            Ok(prepared_input) => prepared_input.with_playback_window(playback_window),
            Err(message) => return AppRouteApplyResult::Failed { message },
        };
        let backend_constraint =
            crate::video_backend_constraint::media_install_video_backend_constraint(
                config.video_backend_preference,
            );
        let installed = match self.app_state.install_prepared_media_strong(
            self.playlist_runtime,
            self.renderer,
            prepared_input,
            desired_intent,
            backend_constraint,
        ) {
            Ok(installed) => installed,
            Err(error) => {
                return media_reconfigure_install_failure(error);
            }
        };
        self.app_state.record_installed_media(&installed);
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
        if routes
            .iter()
            .any(|route| route.route == rustiplayer_settings::AppRuntimeRoute::Playlist)
        {
            self.playlist_runtime
                .preflight_playlist_settings()
                .map_err(|message| SettingsRuntimePreflightFailure {
                    route: rustiplayer_settings::AppRuntimeRoute::Playlist,
                    result: AppRouteApplyResult::Failed { message },
                })?;
        }

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

    fn restore_sidebar_width(&mut self, width_points: crate::ui::sidebar::SidebarWidthPoints) {
        self.app_state.restore_sidebar_width(width_points);
    }

    fn finalize_settings_transaction(&mut self) {
        self.playlist_runtime.finalize_playlist_settings();
    }

    fn apply_playlist_runtime_settings(
        &mut self,
        update: &rustiplayer_settings::PlaylistRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult {
        match self
            .playlist_runtime
            .stage_playlist_settings(update.playlist)
        {
            Ok(true) => AppRouteApplyResult::Applied,
            Ok(false) => AppRouteApplyResult::Noop,
            Err(crate::playlist_runtime::PlaylistSettingsStageError::Failed(message)) => {
                AppRouteApplyResult::Failed { message }
            }
            Err(crate::playlist_runtime::PlaylistSettingsStageError::PartialFailure(message)) => {
                AppRouteApplyResult::PartialFailure { message }
            }
        }
    }

    fn rollback_playlist_runtime_settings(&mut self) -> AppRouteApplyResult {
        match self.playlist_runtime.rollback_playlist_settings() {
            Ok(true) => AppRouteApplyResult::Applied,
            Ok(false) => AppRouteApplyResult::Noop,
            Err(message) => AppRouteApplyResult::Failed { message },
        }
    }

    fn apply_player_runtime_settings(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
    ) -> PlayerRuntimeApplyResult {
        let Some(target_backend_preference) = target_policy.video_backend_preference() else {
            return Err(PlayerRuntimeApplyError::Fatal(
                "player settings route не получил exact destination backend policy".to_owned(),
            ));
        };
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
            let reselect_web_media_stream = media_update
                .affected_settings
                .iter()
                .any(|setting_id| setting_id.as_str() == "player.preferred_video_codec_order");
            let media_result = self.reconfigure_active_media(ActiveMediaReconfigureConfig {
                network: app_config.network,
                web_media: app_config.web_media,
                yt_dlp: app_config.yt_dlp,
                demux: app_config.player.demux,
                preferred_video_codec_order: app_config.player.preferred_video_codec_order,
                video_backend_preference: target_backend_preference,
                reselect_web_media_stream,
                rebuild_remote_source: true,
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
            if let Some(enabled) = update.resume_last_position {
                self.playlist_runtime
                    .set_resume_last_position_enabled(enabled);
            }
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
        target_policy: SettingsRouteTargetPolicy,
    ) -> AppRouteApplyResult {
        let preferred_height_changed = requires_web_media_stream_reselection(affected_settings);
        let network_source_changed = requires_remote_source_rebuild(affected_settings);
        if !preferred_height_changed
            && affected_settings.iter().all(|setting_id| {
                setting_id.as_str().starts_with("yt_dlp.")
                    || setting_id.as_str().starts_with("web_media.")
            })
        {
            return self.app_state.apply_media_service_runtime_settings(update);
        }
        let Some(target_backend_preference) = target_policy.video_backend_preference() else {
            return AppRouteApplyResult::Failed {
                message: "media settings route не получил exact destination backend policy"
                    .to_owned(),
            };
        };

        let mut app_config = self.app_state.committed_app_config();
        app_config.network = update.network.clone();
        app_config.web_media = update.web_media.clone();
        app_config.yt_dlp = update.yt_dlp.clone();
        self.reconfigure_active_media(ActiveMediaReconfigureConfig {
            network: app_config.network,
            web_media: app_config.web_media,
            yt_dlp: app_config.yt_dlp,
            demux: app_config.player.demux,
            preferred_video_codec_order: app_config.player.preferred_video_codec_order,
            video_backend_preference: target_backend_preference,
            reselect_web_media_stream: preferred_height_changed,
            rebuild_remote_source: network_source_changed,
            rebuild_local_source: false,
        })
    }
}
