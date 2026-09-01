//! One-shot telemetry готовности первого пользовательского кадра после открытия media.
//!
//! Модуль принадлежит shell-слою: только он видит одновременно public startup intent,
//! correlated player events и факт успешного surface submission. Внутренности demux,
//! decoder и renderer сюда намеренно не протекают.

use std::time::{Duration, Instant};

use media_core::{MediaTime, TrackKind};
use player_core::{MediaInstanceId, PlaybackResumeIntent, PlayerEvent, PlayerSnapshot, SeekTarget};
use video_present_core::VideoPresentFrameIdentity;

/// Пользовательский путь, который открыл media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupMediaOpenKind {
    /// Media передано через CLI/startup argument.
    Cli,
    /// Media и позиция восстановлены из пользовательского checkpoint-а.
    Restore,
}

/// Точная timeline-цель startup attempt-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupTargetExpectation {
    /// Startup должен показать начало media без скрытого seek-а.
    Beginning,
    /// Startup должен завершить ровно один restore к указанной позиции.
    Restore {
        /// Public target, относительно которого проверяются player events и surface frame.
        target_position: Duration,
    },
}

/// Пользовательское состояние playback, в котором startup считается готовым.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupPlaybackExpectation {
    /// После первого корректного кадра audio должен реально возобновиться.
    Playing,
    /// После первого корректного кадра output остаётся подготовленным, но не запущенным.
    Paused,
}

/// Явное знание composition layer-а о наличии audio в startup media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupAudioExpectation {
    /// Media содержит выбранный audio track; readiness требует audio gate.
    Required,
    /// Composition layer уже положительно доказал, что media не содержит audio.
    NotPresent,
    /// Наличие audio ещё неизвестно; transient video-only snapshot ничего не доказывает.
    Unknown,
}

/// Authoritative результат preparation, который разрешает начальное `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupAudioProof {
    /// Prepared topology положительно содержит audio track.
    Required,
    /// Prepared topology окончательно доказала отсутствие audio track-а.
    NotPresent,
}

/// Authoritative video topology текущего prepared media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupVideoProof {
    /// Prepared topology содержит video track и требует surface presentation.
    Required,
    /// Prepared topology окончательно доказала audio-only media.
    NotPresent,
}

/// Единый preparation handoff для startup consumer gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StartupPreparedConsumerProof {
    /// Authoritative audio presence.
    pub audio: StartupAudioProof,
    /// Authoritative video presence.
    pub video: StartupVideoProof,
}

/// Внутреннее состояние video expectation до получения prepared topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupVideoExpectation {
    Unknown,
    Required,
    NotPresent,
}

/// Самодокументируемое ожидание одного startup attempt-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StartupReadinessExpectation {
    kind: StartupMediaOpenKind,
    target: StartupTargetExpectation,
    playback: StartupPlaybackExpectation,
    audio: StartupAudioExpectation,
}

impl StartupReadinessExpectation {
    /// Собирает frozen startup intent до начала media preparation/network path-а.
    #[must_use]
    pub(crate) const fn new(
        kind: StartupMediaOpenKind,
        target: StartupTargetExpectation,
        playback: StartupPlaybackExpectation,
        audio: StartupAudioExpectation,
    ) -> Self {
        Self {
            kind,
            target,
            playback,
            audio,
        }
    }
}

/// Typed причина, по которой старый startup attempt больше не может публиковать success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupReadinessAbortReason {
    /// Новый startup attempt заменил предыдущий.
    Superseded,
    /// Подготовка источника завершилась ошибкой до установки media.
    PreparationFailed,
    /// Подготовленное media не удалось установить в player.
    InstallationFailed,
    /// Runtime завершает работу.
    Shutdown,
    /// Event принадлежит другому media instance после binding-а.
    MediaInstanceMismatch,
    /// Для startup from beginning наблюдался seek либо restore получил новый seek.
    UnexpectedSeek,
    /// Seek event не совпадает с exact restore target.
    SeekTargetMismatch,
    /// Player начал audio вопреки ожиданию paused startup-а.
    PlaybackExpectationMismatch,
    /// Audio evidence противоречит явному `NotPresent`.
    AudioExpectationMismatch,
    /// Active media завершилось fatal player error-ом.
    PlayerFatalError,
}

/// Состояние одного active startup measurement-а.
#[derive(Debug, Clone, Copy)]
struct ActiveStartupAttempt {
    attempt_id: u64,
    expectation: StartupReadinessExpectation,
    accepted_at: Instant,
    media_instance_id: Option<MediaInstanceId>,
    media_opened_at: Option<Instant>,
    render_generation: Option<u64>,
    video_expectation: StartupVideoExpectation,
    matching_target_frame: Option<MatchingTargetFrame>,
    matching_seek_committed_at: Option<Instant>,
    surface_presented_at: Option<Instant>,
    audio_output_ready_at: Option<Instant>,
    audio_resumed_at: Option<Instant>,
}

/// Correlation evidence target frame-а, опубликованное самим player seek lifecycle.
#[derive(Debug, Clone, Copy)]
struct MatchingTargetFrame {
    frame_pts: Duration,
    observed_at: Instant,
}

/// App-owned one-shot tracker честной process/media-open → surface+audio метрики.
pub(crate) struct StartupReadinessTracker {
    process_started_at: Instant,
    next_attempt_id: u64,
    active_attempt: Option<ActiveStartupAttempt>,
}

impl StartupReadinessTracker {
    /// Создаёт tracker с origin, снятым самым первым кодом `main` до bootstrap-а.
    #[must_use]
    pub(crate) const fn new(process_started_at: Instant) -> Self {
        Self {
            process_started_at,
            next_attempt_id: 1,
            active_attempt: None,
        }
    }

    /// Начинает новое exact startup expectation и терминально supersede-ит старое.
    pub(crate) fn begin_attempt(
        &mut self,
        expectation: StartupReadinessExpectation,
        accepted_at: Instant,
    ) {
        self.abort_attempt(StartupReadinessAbortReason::Superseded, accepted_at);

        let attempt_id = self.next_attempt_id;
        self.next_attempt_id = self.next_attempt_id.saturating_add(1);
        self.active_attempt = Some(ActiveStartupAttempt {
            attempt_id,
            expectation,
            accepted_at,
            media_instance_id: None,
            media_opened_at: None,
            render_generation: None,
            video_expectation: StartupVideoExpectation::Unknown,
            matching_target_frame: None,
            matching_seek_committed_at: None,
            surface_presented_at: None,
            audio_output_ready_at: None,
            audio_resumed_at: None,
        });

        tracing::info!(
            startup_attempt_id = attempt_id,
            process_elapsed_ms = elapsed_milliseconds(self.process_started_at, accepted_at),
            media_open_kind = ?expectation.kind,
            startup_target = ?expectation.target,
            playback_expectation = ?expectation.playback,
            audio_expectation = ?expectation.audio,
            "Startup media-open/restore accepted"
        );
    }

    /// Терминально инвалидирует active attempt и очищает все накопленные gates.
    pub(crate) fn abort_attempt(
        &mut self,
        reason: StartupReadinessAbortReason,
        observed_at: Instant,
    ) {
        let Some(aborted_attempt) = self.active_attempt.take() else {
            return;
        };

        tracing::debug!(
            startup_attempt_id = aborted_attempt.attempt_id,
            process_elapsed_ms = elapsed_milliseconds(self.process_started_at, observed_at),
            media_elapsed_ms = elapsed_milliseconds(aborted_attempt.accepted_at, observed_at),
            ?reason,
            "Startup readiness attempt aborted"
        );
    }

    /// Применяет authoritative audio proof текущего preparation до media binding-а.
    ///
    /// Caller обязан сначала отфильтровать stale async result своей orchestration generation.
    /// Snapshot player-а не имеет права вызывать этот метод для transient video-only tracks.
    pub(crate) fn note_prepared_audio_proof(
        &mut self,
        proof: StartupAudioProof,
        observed_at: Instant,
    ) {
        let contradiction = {
            let Some(attempt) = self.active_attempt.as_mut() else {
                return;
            };
            match (attempt.expectation.audio, proof) {
                (StartupAudioExpectation::Unknown, StartupAudioProof::Required) => {
                    attempt.expectation.audio = StartupAudioExpectation::Required;
                    false
                }
                (StartupAudioExpectation::Unknown, StartupAudioProof::NotPresent) => {
                    attempt.expectation.audio = StartupAudioExpectation::NotPresent;
                    false
                }
                (StartupAudioExpectation::Required, StartupAudioProof::Required)
                | (StartupAudioExpectation::NotPresent, StartupAudioProof::NotPresent) => false,
                (StartupAudioExpectation::Required, StartupAudioProof::NotPresent)
                | (StartupAudioExpectation::NotPresent, StartupAudioProof::Required) => true,
            }
        };

        if contradiction {
            self.abort_attempt(
                StartupReadinessAbortReason::AudioExpectationMismatch,
                observed_at,
            );
        } else {
            self.finish_if_ready(observed_at);
        }
    }

    /// Применяет единый authoritative consumer proof до install barrier-а.
    pub(crate) fn note_prepared_consumer_proof(
        &mut self,
        proof: StartupPreparedConsumerProof,
        observed_at: Instant,
    ) {
        let Some(attempt) = self.active_attempt.as_mut() else {
            return;
        };
        attempt.video_expectation = match proof.video {
            StartupVideoProof::Required => StartupVideoExpectation::Required,
            StartupVideoProof::NotPresent => StartupVideoExpectation::NotPresent,
        };
        self.note_prepared_audio_proof(proof.audio, observed_at);
    }

    /// Принимает correlated player event без вывода identity из source label.
    pub(crate) fn note_player_event(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        event: &PlayerEvent,
        observed_at: Instant,
    ) {
        match event {
            PlayerEvent::MediaOpened(_) => {
                self.note_media_opened(media_instance_id, observed_at);
            }
            PlayerEvent::SeekRequested(request) => {
                self.note_seek_requested(media_instance_id, request.target, observed_at);
            }
            PlayerEvent::SeekTargetFramePresented(presentation) => {
                self.note_target_frame(
                    media_instance_id,
                    presentation.target_position,
                    presentation.frame_pts,
                    observed_at,
                );
            }
            PlayerEvent::SeekCommitted(commit) => {
                self.note_seek_committed(
                    media_instance_id,
                    commit.target_position,
                    commit.resume_intent,
                    observed_at,
                );
            }
            PlayerEvent::AudioResumedAfterSeek(resume) => {
                self.note_seek_audio_resumed(
                    media_instance_id,
                    resume.target_position,
                    observed_at,
                );
            }
            PlayerEvent::AudioTrackSelected(_) => {
                self.note_positive_audio_evidence(media_instance_id, observed_at);
            }
            PlayerEvent::AudioOutputReady => {
                self.note_audio_output_ready(media_instance_id, observed_at);
            }
            PlayerEvent::AudioPlaybackResumed => {
                self.note_audio_playback_resumed(media_instance_id, observed_at);
            }
            PlayerEvent::FatalError(_) => {
                if self.event_matches_bound_media(media_instance_id) {
                    self.abort_attempt(StartupReadinessAbortReason::PlayerFatalError, observed_at);
                }
            }
            PlayerEvent::ShutdownRequested => {
                self.abort_attempt(StartupReadinessAbortReason::Shutdown, observed_at);
            }
            PlayerEvent::MediaOpenRequested(_)
            | PlayerEvent::PlaybackStateChanged(_)
            | PlayerEvent::PositionChanged(_)
            | PlayerEvent::VideoFrameReady(_)
            | PlayerEvent::BufferingStateChanged(_)
            | PlayerEvent::CapabilityScanCompleted(_)
            | PlayerEvent::VideoTrackSelected(_)
            | PlayerEvent::VideoBackendSelectionRequested(_)
            | PlayerEvent::SubtitleTrackSelected(_)
            | PlayerEvent::QualitySelectionChanged(_)
            | PlayerEvent::ConfigReloadRequested
            | PlayerEvent::RecoverableError(_) => {}
        }
    }

    /// Уточняет consumer expectations только положительным наличием track-а.
    pub(crate) fn reconcile_tracks(&mut self, snapshot: &PlayerSnapshot, observed_at: Instant) {
        let has_audio_track = snapshot
            .tracks
            .iter()
            .any(|track| track.kind == TrackKind::Audio);
        let has_video_track = snapshot
            .tracks
            .iter()
            .any(|track| track.kind == TrackKind::Video);
        let Some(attempt) = self.matching_attempt_mut(snapshot.media_instance_id) else {
            return;
        };
        attempt.render_generation = Some(snapshot.render_generation);
        if has_video_track {
            attempt.video_expectation = StartupVideoExpectation::Required;
        }

        if has_audio_track {
            self.note_positive_audio_evidence(snapshot.media_instance_id, observed_at);
        }
    }

    /// Фиксирует кадр только после реального surface presentation в shell renderer-е.
    pub(crate) fn note_surface_frame_presented(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        frame_identity: VideoPresentFrameIdentity,
        render_generation: u64,
        observed_at: Instant,
    ) {
        {
            let Some(attempt) = self.matching_attempt_mut(media_instance_id) else {
                return;
            };

            if attempt.render_generation != Some(render_generation)
                || frame_identity.render_generation() != render_generation
            {
                return;
            }

            match attempt.expectation.target {
                StartupTargetExpectation::Beginning => {}
                StartupTargetExpectation::Restore { target_position } => {
                    let Some(target_frame) = attempt.matching_target_frame else {
                        return;
                    };
                    if frame_identity.pts() < target_position
                        || frame_identity.pts() < target_frame.frame_pts
                    {
                        return;
                    }
                }
            }
        }

        let process_started_at = self.process_started_at;
        let Some(attempt) = self.matching_attempt_mut(media_instance_id) else {
            return;
        };
        if attempt.surface_presented_at.is_none() {
            attempt.surface_presented_at = Some(observed_at);
            tracing::info!(
                startup_attempt_id = attempt.attempt_id,
                process_to_presented_ms = elapsed_milliseconds(process_started_at, observed_at),
                media_to_presented_ms = elapsed_milliseconds(attempt.accepted_at, observed_at),
                frame_pts_ms = frame_identity.pts().as_millis(),
                "First startup video frame presented"
            );
        }

        self.finish_if_ready(observed_at);
    }

    fn note_media_opened(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        observed_at: Instant,
    ) {
        let Some(media_instance_id) = media_instance_id else {
            return;
        };
        let Some(attempt) = self.active_attempt.as_mut() else {
            return;
        };

        match attempt.media_instance_id {
            None => {
                attempt.media_instance_id = Some(media_instance_id);
                attempt.media_opened_at = Some(observed_at);
            }
            Some(bound_media_instance_id) if bound_media_instance_id == media_instance_id => {}
            Some(_) => self.abort_attempt(
                StartupReadinessAbortReason::MediaInstanceMismatch,
                observed_at,
            ),
        }
    }

    fn note_seek_requested(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        target: SeekTarget,
        observed_at: Instant,
    ) {
        let abort_reason = {
            let Some(attempt) = self.matching_attempt_mut(media_instance_id) else {
                return;
            };
            match attempt.expectation.target {
                StartupTargetExpectation::Beginning => {
                    Some(StartupReadinessAbortReason::UnexpectedSeek)
                }
                StartupTargetExpectation::Restore { target_position }
                    if seek_target_matches(target, target_position) =>
                {
                    None
                }
                StartupTargetExpectation::Restore { .. } => {
                    Some(StartupReadinessAbortReason::SeekTargetMismatch)
                }
            }
        };

        if let Some(reason) = abort_reason {
            self.abort_attempt(reason, observed_at);
        }
    }

    fn note_target_frame(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        target_position: Duration,
        frame_pts: Duration,
        observed_at: Instant,
    ) {
        let abort_reason = {
            let Some(attempt) = self.matching_attempt_mut(media_instance_id) else {
                return;
            };
            match attempt.expectation.target {
                StartupTargetExpectation::Beginning => {
                    Some(StartupReadinessAbortReason::UnexpectedSeek)
                }
                StartupTargetExpectation::Restore {
                    target_position: expected_target,
                } if target_position == expected_target && frame_pts >= expected_target => {
                    attempt
                        .matching_target_frame
                        .get_or_insert(MatchingTargetFrame {
                            frame_pts,
                            observed_at,
                        });
                    None
                }
                StartupTargetExpectation::Restore { .. } => {
                    Some(StartupReadinessAbortReason::SeekTargetMismatch)
                }
            }
        };

        if let Some(reason) = abort_reason {
            self.abort_attempt(reason, observed_at);
        }
    }

    fn note_seek_committed(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        target_position: Duration,
        resume_intent: PlaybackResumeIntent,
        observed_at: Instant,
    ) {
        let abort_reason = {
            let Some(attempt) = self.matching_attempt_mut(media_instance_id) else {
                return;
            };
            match attempt.expectation.target {
                StartupTargetExpectation::Beginning => {
                    Some(StartupReadinessAbortReason::UnexpectedSeek)
                }
                StartupTargetExpectation::Restore {
                    target_position: expected_target,
                } if target_position != expected_target => {
                    Some(StartupReadinessAbortReason::SeekTargetMismatch)
                }
                StartupTargetExpectation::Restore { .. }
                    if !resume_intent_matches(attempt.expectation.playback, resume_intent) =>
                {
                    Some(StartupReadinessAbortReason::PlaybackExpectationMismatch)
                }
                StartupTargetExpectation::Restore { .. } => {
                    attempt
                        .matching_seek_committed_at
                        .get_or_insert(observed_at);
                    None
                }
            }
        };

        if let Some(reason) = abort_reason {
            self.abort_attempt(reason, observed_at);
        } else {
            self.finish_if_ready(observed_at);
        }
    }

    fn note_seek_audio_resumed(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        target_position: Duration,
        observed_at: Instant,
    ) {
        let abort_reason = {
            let Some(attempt) = self.matching_attempt_mut(media_instance_id) else {
                return;
            };
            match attempt.expectation.target {
                StartupTargetExpectation::Beginning => {
                    Some(StartupReadinessAbortReason::UnexpectedSeek)
                }
                StartupTargetExpectation::Restore {
                    target_position: expected_target,
                } if target_position != expected_target => {
                    Some(StartupReadinessAbortReason::SeekTargetMismatch)
                }
                StartupTargetExpectation::Restore { .. }
                    if attempt.expectation.playback == StartupPlaybackExpectation::Paused =>
                {
                    Some(StartupReadinessAbortReason::PlaybackExpectationMismatch)
                }
                StartupTargetExpectation::Restore { .. } => None,
            }
        };

        if let Some(reason) = abort_reason {
            self.abort_attempt(reason, observed_at);
        }
    }

    fn note_positive_audio_evidence(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        observed_at: Instant,
    ) {
        if self.matching_attempt_mut(media_instance_id).is_some() {
            self.note_prepared_audio_proof(StartupAudioProof::Required, observed_at);
        }
    }

    fn note_audio_output_ready(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        observed_at: Instant,
    ) {
        self.note_positive_audio_evidence(media_instance_id, observed_at);
        let process_started_at = self.process_started_at;
        let Some(attempt) = self.matching_attempt_mut(media_instance_id) else {
            return;
        };

        if attempt.audio_output_ready_at.is_none() {
            attempt.audio_output_ready_at = Some(observed_at);
            tracing::info!(
                startup_attempt_id = attempt.attempt_id,
                process_to_audio_output_ms =
                    elapsed_milliseconds(process_started_at, observed_at),
                media_to_audio_output_ms =
                    elapsed_milliseconds(attempt.accepted_at, observed_at),
                playback_expectation = ?attempt.expectation.playback,
                "Startup audio output ready"
            );
        }

        self.finish_if_ready(observed_at);
    }

    fn note_audio_playback_resumed(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        observed_at: Instant,
    ) {
        self.note_positive_audio_evidence(media_instance_id, observed_at);
        let process_started_at = self.process_started_at;
        let playback_expectation = self
            .matching_attempt_mut(media_instance_id)
            .map(|attempt| attempt.expectation.playback);

        if playback_expectation == Some(StartupPlaybackExpectation::Paused) {
            self.abort_attempt(
                StartupReadinessAbortReason::PlaybackExpectationMismatch,
                observed_at,
            );
            return;
        }

        let Some(attempt) = self.matching_attempt_mut(media_instance_id) else {
            return;
        };
        if attempt.audio_resumed_at.is_none() {
            attempt.audio_resumed_at = Some(observed_at);
            tracing::info!(
                startup_attempt_id = attempt.attempt_id,
                process_to_audio_ms = elapsed_milliseconds(process_started_at, observed_at),
                media_to_audio_ms = elapsed_milliseconds(attempt.accepted_at, observed_at),
                "Startup audio playback resumed"
            );
        }

        self.finish_if_ready(observed_at);
    }

    fn finish_if_ready(&mut self, observed_at: Instant) {
        let Some(attempt) = self.active_attempt.as_ref() else {
            return;
        };

        let position_ready = match (attempt.video_expectation, attempt.expectation.target) {
            (StartupVideoExpectation::Unknown, _) => false,
            (StartupVideoExpectation::NotPresent, StartupTargetExpectation::Beginning) => true,
            (StartupVideoExpectation::NotPresent, StartupTargetExpectation::Restore { .. }) => {
                attempt.matching_seek_committed_at.is_some()
            }
            (StartupVideoExpectation::Required, StartupTargetExpectation::Beginning) => {
                attempt.surface_presented_at.is_some()
            }
            (StartupVideoExpectation::Required, StartupTargetExpectation::Restore { .. }) => {
                attempt.matching_target_frame.is_some()
                    && attempt.matching_seek_committed_at.is_some()
                    && attempt.surface_presented_at.is_some()
            }
        };
        let audio_ready = match (attempt.expectation.audio, attempt.expectation.playback) {
            (StartupAudioExpectation::NotPresent, _) => true,
            (StartupAudioExpectation::Required, StartupPlaybackExpectation::Paused) => {
                attempt.audio_output_ready_at.is_some()
            }
            (StartupAudioExpectation::Required, StartupPlaybackExpectation::Playing) => {
                attempt.audio_resumed_at.is_some()
            }
            (StartupAudioExpectation::Unknown, _) => false,
        };
        if !position_ready || !audio_ready {
            return;
        }

        let completed_attempt = self
            .active_attempt
            .take()
            .expect("готовность проверена для существующего startup attempt-а");
        if completed_attempt.expectation.audio == StartupAudioExpectation::NotPresent {
            tracing::info!(
                startup_attempt_id = completed_attempt.attempt_id,
                process_to_audio_ms =
                    elapsed_milliseconds(self.process_started_at, completed_attempt.accepted_at),
                media_to_audio_ms = 0_u128,
                "Startup audio gate not required"
            );
        }
        tracing::info!(
            startup_attempt_id = completed_attempt.attempt_id,
            process_to_ready_ms = elapsed_milliseconds(self.process_started_at, observed_at),
            media_to_ready_ms = elapsed_milliseconds(completed_attempt.accepted_at, observed_at),
            media_open_to_ready_ms = ?completed_attempt
                .media_opened_at
                .map(|opened_at| elapsed_milliseconds(opened_at, observed_at)),
            target_event_to_ready_ms = ?completed_attempt
                .matching_target_frame
                .map(|target_frame| elapsed_milliseconds(target_frame.observed_at, observed_at)),
            media_open_kind = ?completed_attempt.expectation.kind,
            startup_target = ?completed_attempt.expectation.target,
            playback_expectation = ?completed_attempt.expectation.playback,
            audio_expectation = ?completed_attempt.expectation.audio,
            video_expectation = ?completed_attempt.video_expectation,
            "Startup presentation and audio gates ready"
        );
    }

    fn matching_attempt_mut(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
    ) -> Option<&mut ActiveStartupAttempt> {
        let media_instance_id = media_instance_id?;
        let attempt = self.active_attempt.as_mut()?;
        (attempt.media_instance_id == Some(media_instance_id)).then_some(attempt)
    }

    fn event_matches_bound_media(&self, media_instance_id: Option<MediaInstanceId>) -> bool {
        let Some(attempt) = self.active_attempt.as_ref() else {
            return false;
        };
        attempt.media_instance_id.is_some() && attempt.media_instance_id == media_instance_id
    }

    #[cfg(test)]
    fn has_active_attempt(&self) -> bool {
        self.active_attempt.is_some()
    }
}

fn seek_target_matches(target: SeekTarget, expected_target: Duration) -> bool {
    matches!(
        target,
        SeekTarget::Absolute(position)
            if position == MediaTime::from_duration(expected_target)
    )
}

fn resume_intent_matches(
    expectation: StartupPlaybackExpectation,
    resume_intent: PlaybackResumeIntent,
) -> bool {
    matches!(
        (expectation, resume_intent),
        (
            StartupPlaybackExpectation::Playing,
            PlaybackResumeIntent::Play
        ) | (
            StartupPlaybackExpectation::Paused,
            PlaybackResumeIntent::Pause
        )
    )
}

fn elapsed_milliseconds(started_at: Instant, observed_at: Instant) -> u128 {
    observed_at
        .saturating_duration_since(started_at)
        .as_millis()
}

#[cfg(test)]
#[path = "startup_readiness/tests.rs"]
mod tests;
