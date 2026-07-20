use media_core::{MediaDuration, MediaTime};

use crate::MediaPlaybackWindow;

use super::*;

impl PlayerSession {
    /// В одном infallible owner turn меняет media/decoder/generation/clocks/pipeline state.
    ///
    /// Метод вызывается под D52 playback-intent mutex: exact-instance updates не могут
    /// пересечь ownership switch наполовину. После возврата protocol синхронно публикует
    /// Installed с той же applied revision.
    pub(crate) fn commit_staged_media(
        &mut self,
        prepared_commit: PreparedStagedMediaCommit,
        accepted_intent: AcceptedPlaybackIntent,
    ) {
        let PreparedStagedMediaCommit {
            prepared_media,
            audio_plan,
            video_plan,
            media_instance_id,
            started_video_backend,
            defer_video_backend_to_compatibility_adapter,
        } = prepared_commit;

        let playback_window = prepared_media.playback_window();
        let source_duration = prepared_media.duration();
        let public_duration = playback_window.map_or(source_duration, |window| {
            window
                .relative_duration(source_duration)
                .map(MediaDuration::as_duration)
        });
        let media_summary = MediaSummary {
            title: prepared_media.media_title(),
            source_label: prepared_media.source_label(),
            duration: public_duration,
        };
        let media_open_request = MediaOpenRequest::new(
            prepared_media.media_source(),
            accepted_intent.intent == PlaybackIntent::StartPlaying,
        );
        let seekability = prepared_media.seekability();
        let playback_window_start = playback_window
            .map(MediaPlaybackWindow::start)
            .unwrap_or(MediaTime::ZERO)
            .as_duration();
        let retired_render_generation = self.pipeline.render_generation();
        let frame_releases = self.prepare_retired_video_frame_releases();
        let retired_resources = self.pipeline.retire_media_resource_owners();
        let backend_id = started_video_backend
            .as_ref()
            .map(|backend| backend.backend_id().to_owned());
        let decoder_thread = started_video_backend.map(StartedVideoBackend::into_decoder_thread);

        self.pipeline.reset_media_slots();
        self.pipeline.install_staged_video_decoder(decoder_thread);
        self.active_video_backend_id = backend_id;
        self.pipeline.advance_render_generation();

        let crate::media_opening::PreparedMediaSlots {
            demuxer,
            file_path,
            source_label,
            tracks,
            source_info,
            playback_window: installed_playback_window,
        } = prepared_media.into_pipeline_slots();
        self.pipeline
            .install_opened_media(demuxer, file_path, source_label, tracks);
        self.pipeline.update_media_source_info(source_info);
        self.reset_session_state_for_staged_media_commit();
        debug_assert_eq!(playback_window, installed_playback_window);
        self.playback_window = installed_playback_window;
        self.reset_playback_window_end_observation();

        if let Some(audio_plan) = audio_plan {
            self.pipeline
                .install_deferred_audio_decoder_config(audio_plan.decoder_config);
            self.pipeline.select_audio_track(audio_plan.track_id);
        }
        if let Some(video_plan) = video_plan {
            if defer_video_backend_to_compatibility_adapter {
                self.pipeline.select_video_track_with_frame_contract(
                    video_plan.track_id,
                    video_plan.requirement.clone(),
                    video_plan.frame_contract,
                );
                self.request_video_backend_reselection(video_plan.requirement, video_plan.track_id);
            } else {
                self.pipeline.select_video_track_with_frame_contract(
                    video_plan.track_id,
                    video_plan.requirement,
                    video_plan.frame_contract,
                );
            }
        }

        self.snapshot.media_instance_id = Some(media_instance_id);
        self.snapshot.media_title = media_summary.title.clone();
        self.snapshot.source_label = Some(media_summary.source_label.clone());
        self.set_snapshot_duration(source_duration);
        self.apply_demux_seekability(seekability);
        self.reset_playback_rate_for_media_load();
        self.snapshot.selected_tracks.audio_track = self.pipeline.selected_audio_track_id();
        self.snapshot.selected_tracks.video_track = self.pipeline.selected_video_track_id();
        let committed_playback_state = match accepted_intent.intent {
            PlaybackIntent::StartPlaying => PlaybackState::Buffering,
            PlaybackIntent::StartPaused => PlaybackState::Paused,
        };
        self.set_playback_state(committed_playback_state);
        self.current_source_position = playback_window_start;
        self.snapshot.set_timeline_position(MediaTime::ZERO);
        self.pipeline.set_media_clock_base(playback_window_start);
        self.pipeline
            .reset_audio_clock_sample(playback_window_start, Instant::now());
        self.clear_error();
        self.push_player_event(PlayerEvent::MediaOpenRequested(media_open_request));
        self.push_player_event(PlayerEvent::MediaOpened(media_summary));

        self.release_retired_video_frames(frame_releases, &retired_resources);
        let retired_decoder = retired_resources.release_non_video_owners_and_take_decoder();
        self.pipeline
            .retain_retired_video_decoder_for_outstanding_leases(
                retired_render_generation,
                retired_decoder,
            );
    }

    /// Применяет D52 intent только к exact current instance, созданному matching request-ом.
    ///
    /// Ошибка audio/runtime boundary не игнорируется: exact instance переводится в runtime
    /// failure, но correlation outcome остаётся AppliedToInstalled — request не стал stale.
    pub(crate) fn apply_playback_intent_to_exact_installed_instance(
        &mut self,
        media_instance_id: MediaInstanceId,
        intent: PlaybackIntent,
    ) -> bool {
        if self.snapshot.media_instance_id != Some(media_instance_id) {
            return false;
        }

        let apply_result = match intent {
            PlaybackIntent::StartPlaying => self.dispatch_command(crate::PlayerCommand::Play),
            PlaybackIntent::StartPaused => self.dispatch_command(crate::PlayerCommand::Pause),
        };
        if let Err(error) = apply_result {
            self.set_runtime_error(format!(
                "Exact playback intent для media instance {media_instance_id} не применён: {error}"
            ));
        }
        true
    }

    /// Извлекает старые frames и фиксирует их release paths до смены generation.
    fn prepare_retired_video_frame_releases(&mut self) -> RetiredVideoFrameReleases {
        let mut resource_handles = self.pipeline.clear_video_queues();
        if let Some(frame) = self.pipeline.clear_seek_preroll_fallback_video_frame() {
            resource_handles.push(frame.resource_handle);
        }
        if let Some(frame) = self.pipeline.take_present_video_frame() {
            resource_handles.push(frame.resource_handle);
        }

        let mut releases = RetiredVideoFrameReleases::default();
        for resource_handle in resource_handles {
            match self.pipeline.request_video_texture_release(resource_handle) {
                VideoTextureReleaseEffect::DeferredUntilRenderLeaseDrop => {}
                VideoTextureReleaseEffect::ReleaseViaRenderProvider(resource_provider) => {
                    releases
                        .provider_releases
                        .push((resource_handle, resource_provider));
                }
                VideoTextureReleaseEffect::ReleaseNow => {
                    releases.decoder_handles.push(resource_handle);
                }
            }
        }
        releases
    }

    /// Освобождает frame resources только после установки нового ownership state.
    fn release_retired_video_frames(
        &self,
        releases: RetiredVideoFrameReleases,
        retired_resources: &crate::pipeline::RetiredMediaResourceOwners,
    ) {
        for resource_handle in releases.decoder_handles {
            retired_resources.release_video_frame(resource_handle);
        }
        for (resource_handle, resource_provider) in releases.provider_releases {
            resource_provider.release_frame(resource_handle);
        }
    }

    /// Очищает session-owned media state без fallible decoder/audio lifecycle calls.
    fn reset_session_state_for_staged_media_commit(&mut self) {
        self.reset_diagnostics_for_media();
        self.clear_pending_video_backend_reselection();
        self.media_lifecycle.clear_pending_autoplay();
        self.seek_runtime.clear_active_commit();
        self.prepared_seek_landing.clear_promoted_seek_ownership();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_simple_scrub();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.snapshot.clear_timeline();
        self.current_source_position = Duration::ZERO;
        self.source_duration = None;
        self.playback_window = None;
        self.reset_playback_window_end_observation();
        self.snapshot.selected_tracks = TrackSelectionSnapshot::default();
        self.snapshot.tracks.clear();
        self.last_audio_starvation_warn_at = None;
        self.last_seen_audio_underrun_callbacks = 0;
        self.last_tick_observed_at = None;
    }
}
