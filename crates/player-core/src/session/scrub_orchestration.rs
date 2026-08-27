use std::time::{Duration, Instant};

use frame_server_core::{
    CancelScrubReason, CancelledOutcome, FinishScrubPolicy, LiveScrubDiagnostics,
    ScrubCurrentGuards, ScrubDriverOutcome, ScrubEvent, ScrubGeneration, ScrubGenerationToken,
    ScrubRequestKind, ValidatedFrameServerConfig,
};
use media_core::{
    MediaDuration, MediaTime, TimeBase, TimelineMode, TimelinePreviewState, TrackKind,
    TrackTimestamp,
};
use tracing::{debug, info, warn};

use crate::seek_state::{PlaybackResumeIntent, SeekLandingRoute, VisibleScrubPreview};
use crate::{
    PlaybackState, PlayerError, PlayerErrorKind, PlayerResult, ScrubCommitOutcome,
    ScrubCommitPolicy, SeekMode, SeekRequest, VisibleScrubPreviewUnavailableReason,
};

use super::PlayerSession;
use super::seek_admission::SeekTimelineAdmission;

/// Максимальный шаг вперёд, при котором live scrub продолжает текущий decode-проход
/// вместо нового cold seek на keyframe-before.
///
/// Движение вперёд в пределах этого окна почти всегда дешевле продолжить с текущей
/// позиции декодера (прокат едет только вперёд, без скачка назад на keyframe).
/// Большой прыжок вперёд декодировал бы все промежуточные кадры и стал бы
/// патологически дорогим, поэтому он идёт обычным cold-маршрутом (аналог капа
/// forward extension из hover Сессии 3).
pub(super) const LIVE_SCRUB_FORWARD_EXTENSION_MAX: Duration = Duration::from_secs(3);

/// Собирает намерение запуска reused-decoder SeekLanding в один именованный контракт.
pub(super) struct ReusedDecoderScrubLandingRequest {
    /// Финальная позиция, которую должен показать SeekLanding.
    pub(super) target_position: MediaTime,
    /// Режим demux seek для поиска decode point.
    pub(super) seek_mode: SeekMode,
    /// Намерение восстановить playback после завершения seek.
    pub(super) resume_intent: PlaybackResumeIntent,
    /// Typed источник допуска к seek lifecycle.
    pub(super) timeline_admission: SeekTimelineAdmission,
    /// Маршрут отличает one-shot seek от live scrub preview.
    pub(super) route: SeekLandingRoute,
    /// Диагностика конкретного live scrub запроса, если маршрут её поддерживает.
    pub(super) live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    /// Проверенная конфигурация frame-server для создаваемого driver-а.
    pub(super) config: ValidatedFrameServerConfig,
    /// Политика завершения определяет допустимый визуальный commit.
    pub(super) finish_policy: FinishScrubPolicy,
}

/// Side effects выхода из lightweight scrub после восстановления public state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimpleScrubExitMode {
    /// Только вернуть state; следующая external command сама решит audio/clock route.
    RestoreStateOnly,

    /// Вернуть state и продолжить playback, если scrub начинался из `Playing`.
    ResumeConfirmedPlayback,
}

pub(super) fn initial_scrub_generation_before_target(
    target_scrub_generation: ScrubGeneration,
) -> PlayerResult<ScrubGeneration> {
    let Some(initial_generation) = target_scrub_generation.get().checked_sub(1) else {
        return Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            "SeekLanding target scrub generation cannot be zero",
        ));
    };

    Ok(ScrubGeneration::new(initial_generation))
}

impl PlayerSession {
    pub(super) fn enter_seek_landing_public_scrubbing(&mut self, target_position: MediaTime) {
        self.set_playback_state(PlaybackState::Scrubbing);
        self.set_timeline_target_from_source(target_position);
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();
        self.snapshot.timeline.preview_state = TimelinePreviewState::Pending;
    }

    /// Строит target PTS в timebase выбранного video track-а для neutral scrub events.
    pub(super) fn seek_landing_target_pts(
        &self,
        video_track_id: crate::TrackId,
        target_position: MediaTime,
    ) -> TrackTimestamp {
        let time_base = self
            .pipeline
            .tracks()
            .iter()
            .find(|track| track.id == video_track_id && track.kind == TrackKind::Video)
            .and_then(|track| track.time_base)
            .unwrap_or_else(|| TimeBase::new(1, 1_000).expect("valid millisecond timebase"));
        let units = time_base.duration_to_track_units_saturating(MediaDuration::from_duration(
            target_position.as_duration(),
        ));
        TrackTimestamp::new(video_track_id, units.get(), time_base)
    }

    /// Начинает timeline scrub gesture на уровне public state без немедленного commit-а.
    pub(super) fn begin_scrub(
        &mut self,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let confirmed_playback_state = self.playback_state_for_new_simple_scrub();
        self.enter_simple_scrub_public_state();
        self.seek_runtime
            .begin_simple_scrub(confirmed_playback_state, live_scrub_diagnostics);
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = false;
        self.snapshot.timeline.target_position = Some(self.snapshot.timeline.current_position);
        info!(
            kind = "seek_acceptance",
            current_position_ms = self
                .snapshot
                .timeline
                .current_position
                .as_duration()
                .as_millis(),
            "Player command received command=BeginScrub"
        );
        Ok(())
    }

    /// Запоминает последнюю цель scrub без изменения текущей playback позиции.
    pub(super) fn update_scrub(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.store_simple_scrub_request(request, None)?;
        Ok(())
    }

    /// Сохраняет preview request и запускает live reused-decoder route для timeline drag.
    pub(super) fn preview_scrub(
        &mut self,
        request: SeekRequest,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let target_position = self.resolve_seek_target(request)?;
        let confirmed_playback_state = self
            .seek_runtime
            .simple_scrub_confirmed_playback_state()
            .unwrap_or_else(|| self.playback_state_for_new_simple_scrub());
        let resume_intent = PlaybackResumeIntent::from_playback_state(confirmed_playback_state);
        self.store_simple_scrub_request(request, live_scrub_diagnostics)?;
        let begin_to_preview_ms = self
            .seek_runtime
            .simple_scrub_elapsed()
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or_default();
        info!(
            kind = "seek_acceptance",
            target_ms = target_position.as_duration().as_millis(),
            begin_to_preview_ms,
            "Player command received command=PreviewScrub"
        );
        let live_scrub_diagnostics =
            live_scrub_diagnostics.or_else(|| self.seek_runtime.simple_scrub_live_diagnostics());

        if !self.pipeline.has_demuxer() || !self.pipeline.has_selected_video_track() {
            return Ok(());
        }

        self.start_reused_decoder_scrub_landing_transaction(ReusedDecoderScrubLandingRequest {
            target_position,
            seek_mode: request.mode,
            resume_intent,
            timeline_admission: SeekTimelineAdmission::PublicSeekableRange,
            route: SeekLandingRoute::live_scrub_preview(live_scrub_diagnostics),
            live_scrub_diagnostics,
            config: self.frame_server_config,
            finish_policy: FinishScrubPolicy::CommitVisiblePreview,
        })?;
        Ok(())
    }

    fn store_simple_scrub_request(
        &mut self,
        request: SeekRequest,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> PlayerResult<()> {
        let target_position = self.resolve_seek_target(request)?;
        let confirmed_playback_state = self.playback_state_for_new_simple_scrub();
        self.enter_simple_scrub_public_state();
        self.seek_runtime.store_simple_scrub_request(
            request,
            confirmed_playback_state,
            live_scrub_diagnostics,
        );
        self.snapshot.timeline.scrubbing = true;
        self.set_timeline_target_from_source(target_position);
        if !self.snapshot.timeline.seeking {
            self.snapshot.timeline.stale_frame = false;
        }
        Ok(())
    }

    /// Проверяет stable visible-preview DTO против текущего player-owned route/frame state.
    fn validated_visible_scrub_preview(
        &self,
    ) -> Result<VisibleScrubPreview, VisibleScrubPreviewUnavailableReason> {
        let preview = self
            .seek_runtime
            .visible_scrub_preview()
            .ok_or(VisibleScrubPreviewUnavailableReason::Missing)?;
        let seek_commit = self
            .seek_runtime
            .active_commit()
            .ok_or(VisibleScrubPreviewUnavailableReason::NoActiveLiveScrub)?;
        let current_context = self
            .active_seek_landing_context(seek_commit)
            .filter(|context| context.request_kind() == ScrubRequestKind::LiveScrub)
            .ok_or(VisibleScrubPreviewUnavailableReason::NoActiveLiveScrub)?;

        let current_guards = ScrubCurrentGuards::new(
            current_context.source_revision(),
            current_context.backend_revision(),
            current_context.generation(),
        );
        if let Some(stale_reason) = preview.context.stale_reason_against(current_guards) {
            return Err(VisibleScrubPreviewUnavailableReason::StaleContext(
                stale_reason,
            ));
        }

        if preview.context.track_selection() != current_context.track_selection() {
            return Err(
                VisibleScrubPreviewUnavailableReason::TrackSelectionChanged {
                    preview: preview.context.track_selection(),
                    current: current_context.track_selection(),
                },
            );
        }

        if preview.context.target() != current_context.target()
            || preview.context.exactness_policy() != current_context.exactness_policy()
            || preview.context.request_kind() != current_context.request_kind()
        {
            return Err(VisibleScrubPreviewUnavailableReason::TargetChanged);
        }

        let identity_media_time = MediaTime::from_duration(preview.frame_identity.pts());
        let identity_track_pts = self.seek_landing_target_pts(
            preview.context.track_selection().video_track,
            identity_media_time,
        );
        if preview.timing.media_time != identity_media_time
            || preview.timing.pts != identity_track_pts
        {
            return Err(VisibleScrubPreviewUnavailableReason::TimingIdentityMismatch);
        }

        if self.snapshot.timeline.mode == TimelineMode::Live {
            let available_range = self.snapshot.timeline.seekable_range;
            if !available_range.is_some_and(|range| range.contains(preview.timing.media_time)) {
                return Err(
                    VisibleScrubPreviewUnavailableReason::OutsideLatestLiveRange {
                        preview_position: preview.timing.media_time,
                        available_range,
                    },
                );
            }
        }

        if self.current_present_frame_identity() != Some(preview.frame_identity) {
            return Err(VisibleScrubPreviewUnavailableReason::PresentedFrameChanged);
        }

        Ok(preview)
    }

    /// Запускает exact commit, переиспользуя active live route только при полном совпадении цели.
    fn start_resolved_scrub_commit(
        &mut self,
        request: SeekRequest,
        target: MediaTime,
        resume_intent: PlaybackResumeIntent,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> PlayerResult<()> {
        // Previewed progressive seek не является authoritative final anchor для demuxer-а,
        // который умеет отдельно подтвердить one-shot seek worker receipt-ом. В таком media
        // даже exact same target обязан пройти новый receipted route: иначе EndScrub просто
        // разрешит старому preview-проходу показывать весь preroll до пользовательской цели.
        let active_live_target_matches = !self
            .prepared_demux_seek
            .routes_one_shot_seek_through_worker()
            && self.seek_runtime.active_seek_landing_is_live_scrub()
            && self
                .seek_runtime
                .active_commit()
                .is_some_and(|seek_commit| {
                    seek_commit.target_position == target && seek_commit.seek_mode == request.mode
                });

        if active_live_target_matches {
            self.seek_runtime
                .update_active_live_scrub_diagnostics(live_scrub_diagnostics);
            self.seek_runtime.request_live_scrub_commit(Instant::now());
            self.snapshot.timeline.scrubbing = true;
            self.set_timeline_target_from_source(target);
            return Ok(());
        }

        self.finish_simple_scrub_without_seek(None, SimpleScrubExitMode::RestoreStateOnly);
        self.start_one_shot_seek_landing_from_request(request, resume_intent)
    }

    /// Завершает scrub gesture по выбранной public commit policy.
    pub(super) fn end_scrub(
        &mut self,
        policy: ScrubCommitPolicy,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> PlayerResult<ScrubCommitOutcome> {
        self.ensure_not_shutdown()?;
        self.seek_runtime
            .update_active_live_scrub_diagnostics(live_scrub_diagnostics);
        if !self.seek_runtime.simple_scrub_active() {
            self.finish_simple_scrub_without_seek(None, SimpleScrubExitMode::RestoreStateOnly);
            return Ok(ScrubCommitOutcome::NoActiveGesture);
        }

        let begin_to_end_ms = self
            .seek_runtime
            .simple_scrub_elapsed()
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or_default();
        info!(
            kind = "seek_acceptance",
            begin_to_end_ms, "Player command received command=EndScrub"
        );

        let Some(finished_scrub) = self.seek_runtime.finish_active_simple_scrub() else {
            self.finish_simple_scrub_without_seek(None, SimpleScrubExitMode::RestoreStateOnly);
            return Ok(ScrubCommitOutcome::NoActiveGesture);
        };
        let confirmed_playback_state = finished_scrub.confirmed_playback_state();
        let latest_request = finished_scrub.latest_request();
        let live_scrub_diagnostics =
            live_scrub_diagnostics.or_else(|| finished_scrub.live_scrub_diagnostics());

        let Some(latest_request) = latest_request else {
            self.invalidate_in_flight_scrub_outputs_after_exit("end scrub without target");
            self.finish_simple_scrub_without_seek(
                Some(confirmed_playback_state),
                SimpleScrubExitMode::ResumeConfirmedPlayback,
            );
            return Ok(ScrubCommitOutcome::NoCommitTarget {
                requested_policy: policy,
            });
        };

        let latest_target = self.resolve_seek_target(latest_request)?;
        let resume_intent = PlaybackResumeIntent::from_playback_state(confirmed_playback_state);

        match policy {
            ScrubCommitPolicy::CommitLatestTarget => {
                self.start_resolved_scrub_commit(
                    latest_request,
                    latest_target,
                    resume_intent,
                    live_scrub_diagnostics,
                )?;
                debug!(
                    kind = "seek",
                    target_ms = latest_target.as_duration().as_millis(),
                    "EndScrub: exact latest-target commit запущен"
                );
                Ok(ScrubCommitOutcome::LatestTarget {
                    target: latest_target,
                })
            }
            ScrubCommitPolicy::CommitVisiblePreview => {
                match self.validated_visible_scrub_preview() {
                    Ok(visible_preview) => {
                        let visible_request =
                            SeekRequest::absolute(visible_preview.timing.media_time);
                        self.start_resolved_scrub_commit(
                            visible_request,
                            visible_preview.timing.media_time,
                            resume_intent,
                            live_scrub_diagnostics,
                        )?;
                        debug!(
                            kind = "seek",
                            visible_media_time_ms =
                                visible_preview.timing.media_time.as_duration().as_millis(),
                            visible_pts = ?visible_preview.timing.pts,
                            latest_target_ms = latest_target.as_duration().as_millis(),
                            frame_identity = ?visible_preview.frame_identity,
                            "EndScrub: exact visible-preview commit запущен"
                        );
                        Ok(ScrubCommitOutcome::VisiblePreview {
                            timing: visible_preview.timing,
                            frame_identity: visible_preview.frame_identity,
                        })
                    }
                    Err(reason) => {
                        self.start_resolved_scrub_commit(
                            latest_request,
                            latest_target,
                            resume_intent,
                            live_scrub_diagnostics,
                        )?;
                        debug!(
                            kind = "seek",
                            target_ms = latest_target.as_duration().as_millis(),
                            reason = ?reason,
                            "EndScrub: visible preview недоступен, запущен exact latest-target fallback"
                        );
                        Ok(
                            ScrubCommitOutcome::VisiblePreviewUnavailableFallbackToLatestTarget {
                                target: latest_target,
                                reason,
                            },
                        )
                    }
                }
            }
        }
    }

    /// Сбрасывает только lightweight scrub-флаги, не трогая текущий active seek.
    fn finish_simple_scrub_without_seek(
        &mut self,
        confirmed_playback_state: Option<PlaybackState>,
        exit_mode: SimpleScrubExitMode,
    ) {
        self.seek_runtime.clear_simple_scrub();
        self.snapshot.timeline.scrubbing = false;
        if !self.snapshot.timeline.seeking {
            self.snapshot.timeline.target_position = None;
            self.snapshot.timeline.stale_frame = false;
        }
        if let Some(playback_state) = confirmed_playback_state {
            self.set_playback_state(playback_state);
            if exit_mode == SimpleScrubExitMode::ResumeConfirmedPlayback
                && playback_state == PlaybackState::Playing
            {
                self.resume_audio_and_clock_after_simple_scrub();
            }
        }
    }

    /// Заменяет active SeekLanding новым target-ом без user-cancel semantics.
    pub(super) fn supersede_active_seek_landing_for_new_target(
        &mut self,
    ) -> PlayerResult<Option<ScrubGenerationToken>> {
        if !self.seek_runtime.seek_landing_active() {
            return Ok(None);
        }

        let Some(next_scrub_generation) = self
            .seek_runtime
            .next_seek_landing_scrub_generation_after_supersede()
        else {
            return Err(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                "Nested scrub generation overflow would break SeekLanding replacement guards",
            ));
        };
        let Some(playback_generation) = self.seek_runtime.seek_landing_playback_generation() else {
            return Err(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                "Active SeekLanding is missing scrub playback generation",
            ));
        };
        let next_generation = ScrubGenerationToken::new(playback_generation, next_scrub_generation);
        let release_prepared_ownership_for_cancel =
            !self.seek_runtime.active_seek_landing_is_live_scrub();
        let active_live_scrub_diagnostics =
            self.seek_runtime.active_seek_landing_live_diagnostics();
        let active_context = self
            .seek_runtime
            .active_commit()
            .and_then(|seek_commit| self.active_seek_landing_context(seek_commit));

        debug!(
            target_position = ?self.snapshot.timeline.target_position,
            next_scrub_generation = next_scrub_generation.get(),
            "Superseding active SeekLanding with a new target"
        );

        if let Err(error) = self.clear_active_seek_decoder_output_floor("seek landing superseded") {
            self.mark_fatal_error(error.clone());
            return Err(error);
        }

        if let Some(context) = active_context {
            self.push_scrub_event_with_live_diagnostics(
                ScrubEvent::from_driver_outcome(ScrubDriverOutcome::Cancelled(CancelledOutcome {
                    context,
                    reason: CancelScrubReason::SupersededByNewTarget,
                })),
                active_live_scrub_diagnostics,
            );
        }

        let release_context = release_prepared_ownership_for_cancel
            .then_some(active_context)
            .flatten();
        let _release_outcome = self.release_prepared_seek_landing_for_cancel(
            CancelScrubReason::SupersededByNewTarget,
            release_context,
        );
        self.seek_runtime.clear_active_commit();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_seek_landing();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.pipeline.clear_pending_packets_for_seek();
        self.clear_queued_video_frames();

        Ok(Some(next_generation))
    }

    /// Закрывает live scrub перед внешней командой без commit-а latest target-а.
    pub(super) fn cancel_active_scrub_for_external_command(&mut self, reason: CancelScrubReason) {
        if self.seek_runtime.seek_landing_active() {
            self.cancel_active_seek_landing_for_external_command(reason);
        }

        let Some(finished_scrub) = self.seek_runtime.finish_active_simple_scrub() else {
            return;
        };
        self.invalidate_in_flight_scrub_outputs_after_exit("external command cancel");
        let confirmed_playback_state = finished_scrub.confirmed_playback_state();
        let latest_request = finished_scrub.latest_request();
        debug!(
            reason = ?reason,
            latest_request = ?latest_request,
            confirmed_playback_state = ?confirmed_playback_state,
            "Cancelling active scrub before external command"
        );
        self.finish_simple_scrub_without_seek(
            Some(confirmed_playback_state),
            SimpleScrubExitMode::RestoreStateOnly,
        );
    }

    /// Закрывает active S17A SeekLanding без commit-а uncommitted landing target-а.
    fn cancel_active_seek_landing_for_external_command(&mut self, reason: CancelScrubReason) {
        let resume_intent = self
            .seek_runtime
            .active_seek_landing_resume_intent()
            .unwrap_or(PlaybackResumeIntent::Pause);
        debug!(
            reason = ?reason,
            resume_intent = ?resume_intent,
            target_position = ?self.snapshot.timeline.target_position,
            "Cancelling active SeekLanding before external command"
        );
        let active_context = self
            .seek_runtime
            .active_commit()
            .and_then(|seek_commit| self.active_seek_landing_context(seek_commit));
        if let Err(error) = self.clear_active_seek_decoder_output_floor("seek landing cancel") {
            self.mark_fatal_error(error);
            return;
        }

        self.fail_pending_seek_receipts(PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("SeekLanding отменён внешней командой: {reason:?}"),
        ));
        self.invalidate_in_flight_scrub_outputs_after_exit("seek landing external command cancel");
        self.seek_runtime.clear_active_commit();
        let _release_outcome =
            self.release_prepared_seek_landing_for_cancel(reason, active_context);
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_seek_landing();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.pipeline.clear_pending_packets_for_seek();
        self.clear_queued_video_frames();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();
        self.snapshot.timeline.preview_state = TimelinePreviewState::Inactive;

        match resume_intent {
            PlaybackResumeIntent::Pause => {
                self.pause_audio_output_for_seek();
                self.set_playback_state(PlaybackState::Paused);
            }
            PlaybackResumeIntent::Play => {
                self.set_playback_state(PlaybackState::Playing);
            }
        }
    }

    /// Делает все in-flight scrub packets/frames/readiness старым playback generation.
    pub(super) fn invalidate_in_flight_scrub_outputs_after_exit(
        &mut self,
        exit_reason: &'static str,
    ) {
        let generation = self.pipeline.begin_seek_generation();
        self.clear_seek_preroll_fallback_frame();
        self.clear_queued_video_frames();
        debug!(
            exit_reason,
            generation, "Active Scrubbing exit advanced playback generation"
        );
    }

    /// Входит в public `Scrubbing` и замораживает playback-owned audio/clock только один раз.
    fn enter_simple_scrub_public_state(&mut self) {
        if !self.seek_runtime.simple_scrub_active() {
            self.clear_monotonic_media_clock_anchor(Instant::now());
            self.pause_audio_output_for_seek();
        }
        self.set_playback_state(PlaybackState::Scrubbing);
    }

    /// Возобновляет слышимый playback, если scrub release не запускает seek/command route.
    fn resume_audio_and_clock_after_simple_scrub(&mut self) {
        if let Some(play_result) = self.play_audio_output_with_resume_event() {
            if let Err(error) = play_result {
                warn!(error = %error, "Не удалось запустить audio после scrub");
                self.set_runtime_error(format!("Audio play after scrub error: {error}"));
            }
            let observed_at = Instant::now();
            let audio_now = self.audio_clock_now();
            self.pipeline
                .reset_audio_clock_sample(audio_now, observed_at);
        }
        self.anchor_monotonic_media_clock_if_needed(Instant::now());
    }

    /// Возвращает state, от которого cancel-first команды должны продолжать route.
    fn playback_state_for_new_simple_scrub(&self) -> PlaybackState {
        match self.playback_state() {
            PlaybackState::Scrubbing => PlaybackState::Paused,
            playback_state => playback_state,
        }
    }
}
