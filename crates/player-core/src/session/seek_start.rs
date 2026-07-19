use std::time::{Duration, Instant};

use frame_server_core::{
    CancelScrubReason, FinishScrubPolicy, LiveScrubDiagnostics, ScrubExactnessPolicy,
    ScrubGeneration, ScrubTarget, ScrubTargetUpdate, ScrubTrackSelection,
};
use media_core::{MediaDemuxError, MediaTime, TimelineNotSeekableReason, TimelinePreviewState};
use tracing::{debug, warn};

use crate::seek_state::{
    PlaybackResumeIntent, SeekCommitState, SeekDemuxRequestError, SeekLandingRoute,
    demux_seek_request_for_transaction,
};
use crate::{
    PlaybackState, PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult, SeekMode, SeekRequest,
};

use super::PlayerSession;
use super::prepared_seek::{
    SEEK_LANDING_BACKEND_REVISION_UNTRACKED, SEEK_LANDING_FIRST_SCRUB_GENERATION,
    SEEK_LANDING_SOURCE_REVISION_UNTRACKED, seek_landing_generation_token,
};
use super::scrub_driver::{
    PlayerScrubTransactionDriver, default_scrub_execution_policy, scrub_update_guards_for_owner,
};
use super::scrub_orchestration::{
    LIVE_SCRUB_FORWARD_EXTENSION_MAX, ReusedDecoderScrubLandingRequest,
    initial_scrub_generation_before_target,
};

/// Максимальный шаг вперёд, при котором live scrub продолжает текущий decode-проход
/// вместо нового cold seek на keyframe-before.
///
/// Движение вперёд в пределах этого окна почти всегда дешевле продолжить с текущей
/// позиции декодера (прокат едет только вперёд, без скачка назад на keyframe).
/// Большой прыжок вперёд декодировал бы все промежуточные кадры и стал бы
/// патологически дорогим, поэтому он идёт обычным cold-маршрутом (аналог капа
/// forward extension из hover Сессии 3).
impl PlayerSession {
    pub(super) fn seek(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.fail_pending_seek_receipts(PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            "seek superseded by another timeline command",
        ));
        let replacing_active_seek_landing = self.seek_runtime.seek_landing_active();
        let resume_intent = self
            .seek_runtime
            .active_seek_landing_resume_intent()
            .unwrap_or_else(|| PlaybackResumeIntent::from_playback_state(self.playback_state()));
        if !replacing_active_seek_landing {
            self.cancel_active_scrub_for_external_command(CancelScrubReason::UserCancelled);
        }
        self.start_one_shot_seek_landing_from_request(request, resume_intent)
    }

    /// Единая S17A точка входа для user-visible seek producers.
    ///
    /// Video route идёт через reused-decoder scrub driver; audio-only route остаётся
    /// single seek transaction без второго video decode-а.
    pub(super) fn start_one_shot_seek_landing_from_request(
        &mut self,
        request: SeekRequest,
        resume_intent: PlaybackResumeIntent,
    ) -> PlayerResult<()> {
        let target_position = self.resolve_seek_target(request);
        self.push_player_event(PlayerEvent::SeekRequested(request));
        self.seek_runtime.clear_simple_scrub();
        self.snapshot.timeline.scrubbing = false;

        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Seek невозможен: media pipeline ещё не открыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        if !self.pipeline.has_selected_video_track() {
            return self.start_audio_only_seek_landing_transaction(
                target_position,
                request.mode,
                resume_intent,
            );
        }

        self.start_reused_decoder_scrub_landing_transaction(ReusedDecoderScrubLandingRequest {
            target_position,
            seek_mode: request.mode,
            resume_intent,
            route: SeekLandingRoute::OneShot,
            live_scrub_diagnostics: None,
            config: self.frame_server_config,
            finish_policy: FinishScrubPolicy::CommitVisiblePreview,
        })
    }

    /// Выполняет no-video/audio-only SeekLanding через существующую single seek transaction.
    ///
    /// S17A exact landing frame semantics применимы к video route. Для audio-only
    /// нет landing кадра и нет риска второго video decode-а, поэтому используем
    /// текущие typed commit gates без отдельного public command route-а.
    fn start_audio_only_seek_landing_transaction(
        &mut self,
        target_position: MediaTime,
        seek_mode: SeekMode,
        resume_intent: PlaybackResumeIntent,
    ) -> PlayerResult<()> {
        self.start_seek_transaction(target_position, seek_mode, resume_intent)
    }

    /// Запускает S17A SeekLanding через `frame-server-core` scrub driver поверх
    /// текущего playback decoder-а.
    pub(super) fn start_reused_decoder_scrub_landing_transaction(
        &mut self,
        request: ReusedDecoderScrubLandingRequest,
    ) -> PlayerResult<()> {
        let ReusedDecoderScrubLandingRequest {
            target_position,
            seek_mode,
            resume_intent,
            route,
            live_scrub_diagnostics,
            config,
            finish_policy,
        } = request;
        if !self.snapshot.timeline.seekable {
            let reason = self
                .snapshot
                .timeline
                .not_seekable_reason
                .unwrap_or(TimelineNotSeekableReason::UnknownTimeline);
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Seek невозможен: timeline не seekable ({reason:?})"),
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        let Some(video_track_id) = self.pipeline.selected_video_track_id() else {
            return self.start_audio_only_seek_landing_transaction(
                target_position,
                seek_mode,
                resume_intent,
            );
        };

        if route.is_live_scrub()
            && self.try_extend_active_live_scrub_landing_forward(
                target_position,
                live_scrub_diagnostics,
            )
        {
            return Ok(());
        }

        if let Err(error) =
            demux_seek_request_for_transaction(true, target_position.as_duration(), seek_mode)
        {
            self.record_recoverable_error(player_error_from_seek_demux_request_error(error));
            return Ok(());
        }
        let replacement_generation = self.supersede_active_seek_landing_for_new_target()?;
        let replacing_existing_seek_landing = replacement_generation.is_some();
        let generation_token = match replacement_generation {
            Some(generation) => generation,
            None => {
                let playback_generation = self
                    .pipeline
                    .seek_generation()
                    .checked_add(1)
                    .ok_or_else(|| {
                        PlayerError::new(
                            PlayerErrorKind::RuntimeError,
                            "Seek generation overflow would break SeekLanding stale-frame guards",
                        )
                    })?;
                seek_landing_generation_token(
                    playback_generation,
                    ScrubGeneration::new(SEEK_LANDING_FIRST_SCRUB_GENERATION),
                )
            }
        };
        let generation = generation_token.playback_generation.get();
        let target_scrub_generation = generation_token.scrub_generation;

        self.pause_audio_output_for_seek();
        self.seek_runtime.begin_seek_landing_request(
            generation_token,
            seek_mode,
            resume_intent,
            route,
        );
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.pipeline
            .reset_clocks_for_seek(target_position.as_duration());

        let target = ScrubTarget::new(
            target_position,
            self.seek_landing_target_pts(video_track_id, target_position),
        );
        let track_selection = match self.pipeline.selected_audio_track_id() {
            Some(audio_track_id) => ScrubTrackSelection::with_audio(video_track_id, audio_track_id),
            None => ScrubTrackSelection::video_only(video_track_id),
        };
        self.confirm_prepared_seek_landing_unavailable(
            target_position,
            seek_mode,
            resume_intent,
            generation_token,
            track_selection,
            target,
            route.request_kind(),
            route.commit_allowed(),
            live_scrub_diagnostics,
        )?;

        self.enter_seek_landing_public_scrubbing(target_position);

        let update = ScrubTargetUpdate::new(
            scrub_update_guards_for_owner(
                SEEK_LANDING_SOURCE_REVISION_UNTRACKED,
                SEEK_LANDING_BACKEND_REVISION_UNTRACKED,
                generation,
            ),
            track_selection,
            target,
            ScrubExactnessPolicy::ExactFrame,
            route.request_kind(),
            default_scrub_execution_policy(config, finish_policy),
        );
        let initial_scrub_generation =
            initial_scrub_generation_before_target(target_scrub_generation)?;
        let mut driver = PlayerScrubTransactionDriver::new(config, initial_scrub_generation);
        let run = driver.submit_target_update(self, update);
        for event in run.events {
            self.push_scrub_event_with_live_diagnostics(event, live_scrub_diagnostics);
        }

        if self.seek_landing_decode_active() {
            return Ok(());
        }

        if replacing_existing_seek_landing {
            self.invalidate_in_flight_scrub_outputs_after_exit("seek landing replacement failed");
        }
        self.seek_runtime.clear_active_commit();
        self.clear_prepared_seek_landing_with_diagnostics();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_seek_landing();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = false;
        self.snapshot.timeline.preview_state = TimelinePreviewState::Failed;
        self.set_playback_state(PlaybackState::Paused);
        if self.snapshot.last_error.is_none() {
            self.record_recoverable_error(PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "SeekLanding не смог стартовать reused-decoder scrub route",
            ));
        }
        Ok(())
    }

    /// Продолжает активный live scrub проход к более поздней цели без нового cold seek.
    ///
    /// Реюз позиции декодера: движение вперёд в пределах капа не делает flush
    /// декодера и demux seek на keyframe-before — уже идущий decode-проход просто
    /// доезжает до новой цели, а прокат показывает кадры по пути. Generation,
    /// pending video packets и очередь кадров остаются валидными. Audio runtime
    /// сбрасывается под новый target: уже декодированный PCM от старой цели
    /// очищается, clock base перепривязывается, и trace начинается заново, чтобы
    /// landing новой цели снова опубликовал `SeekTargetFramePresented`.
    ///
    /// Возвращает `true`, если расширение применено и cold route не нужен.
    fn try_extend_active_live_scrub_landing_forward(
        &mut self,
        target_position: MediaTime,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        // Расширять можно только активный live scrub decode-проход до EndScrub:
        // после запрошенного commit-а целью владеет release-логика.
        if !self.seek_runtime.active_seek_landing_is_live_scrub()
            || !self.seek_runtime.seek_landing_decode_active()
            || self.seek_runtime.active_seek_landing_commit_allowed()
        {
            return false;
        }

        // Accurate-политика проката обязана быть активной, иначе окажемся в
        // расширении неподдерживаемого маршрута.
        if !seek_commit.drops_decode_preroll_before_target() {
            return false;
        }

        // EOF drain: demuxer уже не читает — новая цель обязана пройти cold route,
        // который заново seek-ает demuxer и выходит из drain-а.
        if self.is_eof_draining() {
            return false;
        }

        let current_target = seek_commit.target_position.as_duration();
        let new_target = target_position.as_duration();
        if new_target < current_target {
            return false;
        }
        if new_target.saturating_sub(current_target) > LIVE_SCRUB_FORWARD_EXTENSION_MAX {
            return false;
        }

        // Audio под новый target: буфер от старой цели очищается, decoder
        // сбрасывается, clock base перепривязывается. Video decoder не трогаем.
        self.reset_audio_runtime_for_seek_landing(seek_commit.generation);
        self.pipeline.reset_clocks_for_seek(new_target);

        self.seek_runtime.set_active_commit(SeekCommitState {
            target_position,
            started_at: Instant::now(),
            ..seek_commit
        });

        // Trace заново: landing новой цели снова должен эмитить
        // `SeekTargetFramePresented`, иначе UI completion-gate ждёт fallback budget.
        self.seek_runtime.clear_trace();
        self.seek_runtime.begin_trace(seek_commit.generation);
        self.seek_runtime
            .update_active_live_scrub_diagnostics(live_scrub_diagnostics);

        self.enter_seek_landing_public_scrubbing(target_position);

        debug!(
            kind = "seek",
            old_target_ms = current_target.as_millis(),
            new_target_ms = new_target.as_millis(),
            generation = seek_commit.generation,
            "Live scrub: цель расширена вперёд без нового cold seek"
        );
        true
    }

    /// Переводит только public state/snapshot в pending SeekLanding.
    ///
    /// Lifecycle шаги seek generation, queues, clocks и promoted ownership остаются
    /// у вызывающего route-а, чтобы prepared instant commit не публиковал Scrubbing.
    pub(super) fn present_frame_covers_target(&self, target_position: Duration) -> bool {
        if !self.pipeline.has_selected_video_track() {
            return true;
        }

        self.pipeline.present_video_frame_covers(target_position)
    }

    /// Сбрасывает video decoder перед seek и делает flush явной fail-fast границей.
    pub(super) fn reset_video_decoder_for_seek(&self) -> Result<(), PlayerError> {
        self.pipeline
            .flush_video_decoder_thread()
            .map_err(player_error_from_decoder_flush_error)
    }

    /// Завершает seek transaction до demux seek, если decoder не подтвердил flush.
    fn fail_seek_transaction_on_decoder_flush(&mut self, error: PlayerError) {
        self.seek_runtime.clear_active_commit();
        self.clear_prepared_seek_landing_with_diagnostics();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = None;
        self.seek_runtime.clear_simple_scrub();
        self.snapshot.timeline.scrubbing = false;
        self.set_playback_state(PlaybackState::Paused);
        self.record_recoverable_error(error);
    }

    /// Выполняет синхронную часть seek transaction и оставляет commit gates на tick.
    pub(super) fn start_seek_transaction(
        &mut self,
        target_position: MediaTime,
        seek_mode: SeekMode,
        resume_intent: PlaybackResumeIntent,
    ) -> PlayerResult<()> {
        if !self.snapshot.timeline.seekable {
            let reason = self
                .snapshot
                .timeline
                .not_seekable_reason
                .unwrap_or(TimelineNotSeekableReason::UnknownTimeline);
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Seek невозможен: timeline не seekable ({reason:?})"),
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Seek невозможен: media pipeline ещё не открыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        let target_duration = target_position.as_duration();
        let demux_seek_request = match demux_seek_request_for_transaction(
            self.pipeline.has_selected_video_track(),
            target_duration,
            seek_mode,
        ) {
            Ok(request) => request,
            Err(error) => {
                self.record_recoverable_error(player_error_from_seek_demux_request_error(error));
                return Ok(());
            }
        };

        if let Err(error) = self.clear_active_seek_decoder_output_floor("new seek") {
            self.mark_fatal_error(error);
            return Ok(());
        }

        self.pause_audio_output_for_seek();
        if let Err(error) = self.reset_video_decoder_for_seek() {
            self.fail_seek_transaction_on_decoder_flush(error);
            return Ok(());
        }

        self.set_playback_state(PlaybackState::Seeking);
        let generation = self.pipeline.begin_seek_generation();
        let has_video = self.pipeline.has_selected_video_track();

        self.pipeline.clear_pending_packets_for_seek();
        self.pipeline.reset_decoder_state_for_seek(has_video);
        if has_video {
            self.record_video_decoder_bootstrap_started();
        }
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.clear_queued_video_frames();
        self.pipeline.reset_clocks_for_seek(target_duration);
        self.snapshot.timeline.target_position = Some(target_position);
        self.snapshot.timeline.seeking = true;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();

        if let Some(Err(error)) = self.pipeline.reset_audio_decoder() {
            let player_error = PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!("Audio decoder reset failed during seek: {error}"),
            );
            self.record_recoverable_error(player_error);
        }

        if let Some(clear_result) = self.pipeline.clear_audio_output_for_seek(generation) {
            match clear_result {
                Ok(ack_generation) => {
                    self.pipeline.mark_audio_buffer_clear_ack(ack_generation);
                }
                Err(error) => {
                    let player_error = PlayerError::new(
                        PlayerErrorKind::AudioDeviceUnavailable,
                        format!("Audio buffer clear failed during seek: {error}"),
                    );
                    self.record_recoverable_error(player_error);
                }
            }
        } else {
            self.pipeline.mark_audio_buffer_clear_ack(generation);
            self.pipeline.reset_audio_clock();
        }

        let seek_result = {
            debug!(
                kind = "seek",
                target_ms = target_duration.as_millis(),
                demux_mode = ?demux_seek_request.mode,
                generation,
                selected_video_track_id = ?self.pipeline.selected_video_track_id(),
                selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
                "Starting demux seek transaction"
            );
            let Some(seek_result) = self.pipeline.seek_demuxer(demux_seek_request) else {
                self.seek_runtime.clear_trace();
                return Ok(());
            };
            seek_result
        };

        match seek_result {
            Ok(result) => {
                debug!(
                    kind = "seek",
                    target_ms = target_duration.as_millis(),
                    actual_ms = result.actual_position.as_duration().as_millis(),
                    actual_track_timestamp = ?result.actual_track_timestamp,
                    demux_mode = ?demux_seek_request.mode,
                    generation,
                    pipeline_generation = self.pipeline.seek_generation(),
                    selected_video_track_id = ?self.pipeline.selected_video_track_id(),
                    selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
                    "Demux seek transaction accepted"
                );
                self.seek_runtime.begin_trace(generation);
                let seek_commit = SeekCommitState {
                    generation,
                    seek_mode,
                    target_position,
                    actual_position: result.actual_position,
                    started_at: Instant::now(),
                    resume_intent,
                };
                self.reanchor_clocks_after_seek_accept(seek_commit);
                self.seek_runtime.set_active_commit(seek_commit);
                if let Err(error) = self.apply_decoder_output_floor_for_seek(seek_commit) {
                    self.seek_runtime.clear_active_commit();
                    self.clear_prepared_seek_landing_with_diagnostics();
                    self.seek_runtime.clear_trace();
                    self.seek_runtime.clear_simple_scrub();
                    self.seek_runtime.clear_eof_fallback_video_position();
                    self.clear_seek_preroll_fallback_frame();
                    self.mark_fatal_error(error);
                    return Ok(());
                }
                Ok(())
            }
            Err(error) => {
                self.seek_runtime.clear_active_commit();
                self.clear_prepared_seek_landing_with_diagnostics();
                self.seek_runtime.clear_trace();
                self.seek_runtime.clear_simple_scrub();
                self.snapshot.timeline.scrubbing = false;
                self.snapshot.timeline.seeking = false;
                self.snapshot.timeline.stale_frame = false;
                self.set_playback_state(PlaybackState::Paused);
                self.snapshot.timeline.target_position = None;
                let player_error = player_error_from_demux_seek_error(error);
                self.record_recoverable_error(player_error);
                Ok(())
            }
        }
    }

    /// Останавливает audio stream для seek, не меняя high-level playback state.
    pub(super) fn pause_audio_output_for_seek(&mut self) {
        if let Some(Err(error)) = self.pipeline.pause_audio_output_and_capture_clock() {
            warn!(error = %error, "Не удалось остановить audio перед seek");
            self.set_runtime_error(format!("Audio pause before seek error: {error}"));
        }
    }
}

/// Мапит ошибку demux seek в player error без смешивания unavailable/timeout/demux.
fn player_error_from_demux_seek_error(error: anyhow::Error) -> PlayerError {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<MediaDemuxError>()
            .is_some_and(MediaDemuxError::is_seek_unavailable)
    }) {
        return PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("Seek failed: {error}"),
        );
    }

    PlayerError::new(PlayerErrorKind::DemuxError, format!("Seek failed: {error}"))
}

/// Мапит ошибку выбора seek policy до mutating-части transaction-а.
fn player_error_from_seek_demux_request_error(error: SeekDemuxRequestError) -> PlayerError {
    match error {
        SeekDemuxRequestError::UnsupportedSeekMode { mode } => PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("Seek mode {mode:?} пока не поддерживается текущим demux contract"),
        ),
    }
}

/// Мапит failed decoder flush в typed player error без продолжения seek transaction.
fn player_error_from_decoder_flush_error(error: anyhow::Error) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::DecoderFlushFailed,
        format!("Video decoder flush failed before seek: {error}"),
    )
}
