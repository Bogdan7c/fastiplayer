use std::time::Duration;

use frame_server_core::CancelScrubReason;
use media_core::{DemuxSeekability, DemuxTrackListUpdate, TimelineSnapshot};
use tracing::{debug, info, warn};

use crate::media_opening::PreparedMedia;
use crate::seek_state::SeekCommitState;
use crate::{
    AuthorizeInstallCommit, MediaInstallCompletion, MediaInstallControl,
    MediaInstallControlOutcome, MediaInstallReceipt, MediaInstallRequestId, MediaOpenRequest,
    MediaSummary, PlaybackIntent, PlaybackIntentRevision, PlaybackState, PlayerError, PlayerEvent,
    PlayerResult, TrackSelectionSnapshot,
};

use super::PlayerSession;

/// Небольшой state media-opening lifecycle, который не должен становиться владельцем pipeline.
#[derive(Debug, Default)]
pub(super) struct MediaLifecycleState {
    /// Autoplay-флаг принятого open request до успешного `MediaOpened`.
    pending_autoplay: bool,
}

impl MediaLifecycleState {
    /// Запоминает autoplay intent только для open request-а, принятого session state machine.
    fn remember_open_autoplay(&mut self, autoplay: bool) {
        self.pending_autoplay = autoplay;
    }

    /// Сбрасывает pending autoplay при reset, failure или переходе в preroll.
    pub(super) fn clear_pending_autoplay(&mut self) {
        self.pending_autoplay = false;
    }

    /// Возвращает autoplay intent без передачи владения playback lifecycle.
    fn pending_autoplay(&self) -> bool {
        self.pending_autoplay
    }
}

impl PlayerSession {
    /// Загружает уже открытый demuxer для streaming source.
    pub fn load_demuxer(&mut self, label: String, demuxer: Box<dyn media_core::Demuxer + Send>) {
        self.load_demuxer_with_autoplay(label, demuxer, false);
    }

    /// Загружает уже открытый demuxer для streaming source с явной autoplay-политикой.
    pub fn load_demuxer_with_autoplay(
        &mut self,
        label: String,
        demuxer: Box<dyn media_core::Demuxer + Send>,
        autoplay: bool,
    ) {
        let prepared_media = PreparedMedia::from_external_label(label, demuxer);
        self.load_prepared_media_with_autoplay(prepared_media, autoplay);
    }

    /// Устанавливает media, уже открытый shell/container adapter слоем.
    pub fn load_prepared_media(&mut self, prepared_media: PreparedMedia) {
        self.load_prepared_media_with_autoplay(prepared_media, false);
    }

    /// Устанавливает prepared media через единственный strong player install algorithm.
    ///
    /// Этот direct session helper нужен внутренним тестам и compatibility callers. Он выполняет
    /// ready/authorize в одном owner turn, но не создаёт отдельный destructive lifecycle.
    pub fn load_prepared_media_with_autoplay(
        &mut self,
        prepared_media: PreparedMedia,
        autoplay: bool,
    ) {
        let request_id = MediaInstallRequestId::new_unique();
        let (receipt, install_port) = MediaInstallReceipt::new(request_id);
        self.stage_prepared_media_install_compatibility(
            request_id,
            prepared_media,
            PlaybackIntent::from_autoplay(autoplay),
            PlaybackIntentRevision::INITIAL,
            install_port,
        );

        if self.has_staged_media_install() {
            let outcome = self.apply_staged_media_install_control(MediaInstallControl::Authorize(
                AuthorizeInstallCommit { request_id },
            ));
            debug_assert_eq!(
                outcome,
                MediaInstallControlOutcome::AuthorizationAccepted,
                "direct compatibility install обязан auto-authorize matching ready request"
            );
        }

        if let Some(MediaInstallCompletion::Failed { failure, .. }) = receipt.try_take_completion()
        {
            self.record_recoverable_error(failure.error);
        }
    }

    /// Публикует ошибку adapter-а без сброса уже работающего media pipeline-а.
    pub fn fail_media_open_with_error(&mut self, request: MediaOpenRequest, error: PlayerError) {
        let active_open_failed = self.active_open_request_matches(&request);
        self.media_lifecycle.clear_pending_autoplay();

        if active_open_failed {
            self.mark_fatal_error(error);
            return;
        }

        if let Err(shutdown_error) = self.ensure_not_shutdown() {
            self.record_recoverable_error(shutdown_error);
            return;
        }

        self.push_player_event(PlayerEvent::MediaOpenRequested(request));
        self.record_recoverable_error(error);
    }

    /// Применяет lifecycle update от demuxer-а после container discontinuity.
    pub(crate) fn handle_demux_track_list_update(&mut self, track_update: DemuxTrackListUpdate) {
        let DemuxTrackListUpdate { tracks, duration } = track_update;
        if self.seek_runtime.active_seek_landing_is_live_scrub() {
            self.cancel_active_scrub_for_external_command(CancelScrubReason::UserCancelled);
        }

        let active_seek_before_update = self.seek_runtime.active_commit();
        let active_timeline_before_update =
            active_seek_before_update.map(|_| self.snapshot.timeline);
        let pipeline_generation_before_update = self.pipeline.seek_generation();
        let first_post_seek_track_update = self.seek_runtime.record_first_track_list_update();

        info!(
            tracks = tracks.len(),
            duration = ?duration,
            active_seek = active_seek_before_update.is_some(),
            first_post_seek_track_update,
            active_seek_generation_before = ?active_seek_before_update.map(|seek| seek.generation),
            pipeline_generation = pipeline_generation_before_update,
            "Demuxer сообщил обновление track list"
        );

        let preserves_worker_seek_runtime = active_seek_before_update.is_some()
            && first_post_seek_track_update
            && self
                .prepared_demux_seek
                .routes_one_shot_seek_through_worker()
            && !self.prepared_demux_seek.receipt_pending()
            && !self.has_dynamic_timeline_binding()
            && self.pipeline.tracks() == tracks.as_slice();
        if preserves_worker_seek_runtime {
            self.set_snapshot_duration(duration);
            debug!(
                kind = "seek",
                generation = pipeline_generation_before_update,
                tracks = tracks.len(),
                duration = ?duration,
                "Exact post-receipt track topology подтверждена без повторного pipeline reset"
            );
            return;
        }

        self.pause_audio_output_for_seek();
        if let Err(error) = self.reset_video_decoder_for_seek() {
            self.mark_fatal_error(error);
            return;
        }

        let generation = self.pipeline.begin_seek_generation();
        let rebased_active_seek = self.rebase_active_seek_after_track_list_reset(generation);
        if let Some(active_seek) = active_seek_before_update {
            debug!(
                kind = "seek",
                target_ms = active_seek.target_position.as_duration().as_millis(),
                actual_ms = active_seek.actual_position.as_duration().as_millis(),
                active_seek_generation_before = active_seek.generation,
                active_seek_generation_after = rebased_active_seek.map(|seek| seek.generation),
                pipeline_generation_before = pipeline_generation_before_update,
                pipeline_generation_after = generation,
                selected_video_track_id = ?self.pipeline.selected_video_track_id(),
                selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
                tracks = tracks.len(),
                duration = ?duration,
                "Active seek rebased after post-seek TracksChanged/ResetRequired marker"
            );
        }
        self.clear_seek_preroll_fallback_frame();
        self.clear_queued_video_frames();
        self.pipeline.apply_demux_track_list_update(tracks.clone());
        self.pipeline.mark_audio_buffer_clear_ack(generation);
        self.set_snapshot_duration(duration);
        if let Some(timeline_before_update) = active_timeline_before_update {
            self.restore_active_timeline_after_track_list_reset(timeline_before_update);
        }
        self.snapshot.selected_tracks = TrackSelectionSnapshot::default();
        if active_timeline_before_update.is_none() {
            self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();
        }

        self.init_audio_pipeline(&tracks);
        if let Err(error) =
            self.select_default_video_track(&tracks, "Video track не найден после demux reset")
        {
            warn!(error = %error, "Video track rejected after demux track-list update");
            self.mark_fatal_error(error);
        }
    }

    /// Перепривязывает active seek transaction к generation, открытому demux reset-ом.
    fn rebase_active_seek_after_track_list_reset(
        &mut self,
        generation: u64,
    ) -> Option<SeekCommitState> {
        let previous_commit = self.seek_runtime.active_commit()?;
        let rebased_commit = self
            .seek_runtime
            .rebase_active_commit_to_generation(generation)?;
        self.rebase_pending_seek_receipts(previous_commit, rebased_commit);
        Some(rebased_commit)
    }

    /// Возвращает volatile timeline-флаги, которые `set_snapshot_duration` пересоздаёт.
    fn restore_active_timeline_after_track_list_reset(
        &mut self,
        timeline_before_update: TimelineSnapshot,
    ) {
        self.snapshot.timeline.target_position = timeline_before_update.target_position;
        self.snapshot.timeline.seeking = timeline_before_update.seeking;
        self.snapshot.timeline.scrubbing = timeline_before_update.scrubbing;
        self.snapshot.timeline.stale_frame = timeline_before_update.stale_frame;
    }

    /// Отмечает успешное открытие media внешним demux/source слоем.
    pub fn mark_media_opened(&mut self, summary: MediaSummary) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.snapshot.media_title = summary.title.clone();
        self.set_snapshot_duration(summary.duration);
        self.snapshot.source_label = Some(summary.source_label.clone());
        self.clear_error();
        self.push_player_event(PlayerEvent::MediaOpened(summary));

        if self.media_lifecycle.pending_autoplay() {
            self.begin_autoplay_preroll()?;
        } else {
            self.pause()?;
        }

        Ok(())
    }

    /// Полностью сбрасывает состояние текущего media.
    pub fn reset_media_state(&mut self) {
        self.set_playback_state(PlaybackState::Paused);
        self.pipeline.clear_monotonic_media_clock();
        self.clear_video_frames();
        self.advance_render_generation();

        if let Err(error) = self.clear_active_seek_decoder_output_floor("media reset") {
            warn!(error = %error, "Не удалось очистить Accurate seek decoder output floor");
        }

        if let Err(error) = self.pipeline.flush_video_decoder_thread() {
            warn!(error = %error, "Не удалось сбросить video decoder thread");
        }

        match self.pipeline.clear_video_decoder_stream() {
            video_core::VideoStreamConfigResult::AbsentDecoder
            | video_core::VideoStreamConfigResult::Cleared
            | video_core::VideoStreamConfigResult::Unchanged => {}
            video_core::VideoStreamConfigResult::Configured => {
                debug!("Decoder stream clear вернул unexpected Configured outcome");
            }
            video_core::VideoStreamConfigResult::Unsupported(rejection) => {
                warn!(
                    rejection = %rejection,
                    "Decoder stream clear rejected current stream config during media reset"
                );
            }
            video_core::VideoStreamConfigResult::Backpressure(reason) => {
                warn!(
                    reason = %reason,
                    "Decoder stream clear hit control-channel backpressure during media reset"
                );
            }
            video_core::VideoStreamConfigResult::Fatal(error) => {
                warn!(
                    error = %error,
                    "Decoder stream clear failed during media reset"
                );
            }
        }

        self.discard_pending_decoded_video_frames();

        self.pipeline.reset_media_slots();
        self.snapshot.media_instance_id = None;
        self.current_source_position = Duration::ZERO;
        self.source_duration = None;
        self.playback_window = None;
        self.dynamic_timeline = super::dynamic_timeline::DynamicTimelineRuntime::default();
        self.reset_playback_window_end_observation();
        self.reset_diagnostics_for_media();

        self.clear_pending_video_backend_reselection();
        self.media_lifecycle.clear_pending_autoplay();
        self.seek_runtime.clear_active_commit();
        self.prepared_demux_seek.reset();
        self.prepared_seek_landing.clear_promoted_seek_ownership();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_simple_scrub();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.snapshot.source_label = None;
        self.snapshot.media_title = None;
        self.snapshot.clear_timeline();
        self.snapshot.selected_tracks = TrackSelectionSnapshot::default();
        self.snapshot.tracks.clear();
        self.clear_error();
    }

    /// Принимает open request и переводит session в `Opening`.
    pub(super) fn open_media(&mut self, request: MediaOpenRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.reset_playback_rate_for_media_load();
        self.media_lifecycle
            .remember_open_autoplay(request.autoplay);
        self.snapshot.source_label = Some(request.source.label());
        self.snapshot.media_title = None;
        self.snapshot.clear_timeline();
        self.clear_error();
        self.push_player_event(PlayerEvent::MediaOpenRequested(request));
        self.set_playback_state(PlaybackState::Opening);
        Ok(())
    }

    /// Проверяет, относится ли failed-open к уже принятому `Opening` request-у.
    fn active_open_request_matches(&self, request: &MediaOpenRequest) -> bool {
        if self.playback_state() != PlaybackState::Opening {
            return false;
        }

        let request_label = request.source.label();
        self.snapshot.source_label.as_deref() == Some(request_label.as_str())
    }

    /// Применяет seekability demuxer/source stack-а к player timeline.
    pub(super) fn apply_demux_seekability(&mut self, seekability: DemuxSeekability) {
        match seekability {
            DemuxSeekability::Seekable => {}
            DemuxSeekability::NotSeekable { reason } => {
                self.snapshot.timeline.seekable = false;
                self.snapshot.timeline.seekable_range = None;
                self.snapshot.timeline.not_seekable_reason = Some(reason);
            }
        }
    }
}
