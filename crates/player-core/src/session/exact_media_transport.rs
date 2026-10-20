//! Serialized exact-instance restart и neutral Stop внутри player owner-а.

use crate::seek_state::PlaybackResumeIntent;
use crate::{
    ExactMediaTransportAction, ExactMediaTransportFailureStage, ExactMediaTransportOutcome,
    ExactMediaTransportRequest, PlaybackIntent, PlayerError, PlayerErrorKind, PlayerResult,
    SeekRequest,
};

use super::PlayerSession;

impl PlayerSession {
    /// Проверяет instance и выполняет составное действие одним worker-owner turn-ом.
    pub(crate) fn apply_exact_media_transport(
        &mut self,
        request: ExactMediaTransportRequest,
    ) -> ExactMediaTransportOutcome {
        let current_media_instance_id = self.snapshot.media_instance_id;
        if current_media_instance_id != Some(request.media_instance_id) {
            return ExactMediaTransportOutcome::StaleInstance {
                requested_media_instance_id: request.media_instance_id,
                current_media_instance_id,
            };
        }

        match request.action {
            ExactMediaTransportAction::SetPlaybackIntent { intent } => {
                let apply_result = match intent {
                    PlaybackIntent::StartPlaying => self.play(),
                    PlaybackIntent::StartPaused => self.pause(),
                };
                match apply_result {
                    Ok(()) => ExactMediaTransportOutcome::Applied {
                        media_instance_id: request.media_instance_id,
                    },
                    Err(error) => ExactMediaTransportOutcome::Failed {
                        media_instance_id: request.media_instance_id,
                        stage: match intent {
                            PlaybackIntent::StartPlaying => ExactMediaTransportFailureStage::Play,
                            PlaybackIntent::StartPaused => ExactMediaTransportFailureStage::Pause,
                        },
                        error,
                    },
                }
            }
            ExactMediaTransportAction::NeutralStop => {
                self.apply_exact_neutral_stop(request.media_instance_id)
            }
            ExactMediaTransportAction::RestartFromBeginning { intent } => {
                self.apply_exact_restart(request.media_instance_id, intent)
            }
            ExactMediaTransportAction::ResetMedia => match self.stop() {
                Ok(()) => ExactMediaTransportOutcome::Applied {
                    media_instance_id: request.media_instance_id,
                },
                Err(error) => ExactMediaTransportOutcome::Failed {
                    media_instance_id: request.media_instance_id,
                    stage: ExactMediaTransportFailureStage::ResetMedia,
                    error,
                },
            },
        }
    }

    /// Pause предшествует seek: Stopped disposition публикует app только после полного success.
    fn apply_exact_neutral_stop(
        &mut self,
        media_instance_id: crate::MediaInstanceId,
    ) -> ExactMediaTransportOutcome {
        if let Err(error) = self.pause() {
            return ExactMediaTransportOutcome::Failed {
                media_instance_id,
                stage: ExactMediaTransportFailureStage::Pause,
                error,
            };
        }
        if let Err(error) = self.start_exact_seek_to_beginning(PlaybackResumeIntent::Pause) {
            return ExactMediaTransportOutcome::PartiallyApplied {
                media_instance_id,
                completed_stage: ExactMediaTransportFailureStage::Pause,
                failed_stage: ExactMediaTransportFailureStage::SeekToBeginning,
                error,
            };
        }
        ExactMediaTransportOutcome::Applied { media_instance_id }
    }

    /// Перезапуск с начала: итоговое Playing/Paused передаётся самой seek-транзакции.
    ///
    /// Намерение задаётся как resume intent самого seek-а и применяется штатным commit-ом
    /// landing-а. Раньше здесь после seek-а вызывались `play()`/`pause()`, и они отменяли
    /// только что начатую SeekLanding как внешнюю команду: demuxer и decoder уже стояли в
    /// нуле, а часы и позиция — на старом месте (Previous замораживал видео). С 2026-10-20
    /// Play/Pause во время одноразового seek-а сами только меняют его resume intent
    /// (`session/seek_resume_intent.rs`), но restart всё равно передаёт intent напрямую.
    fn apply_exact_restart(
        &mut self,
        media_instance_id: crate::MediaInstanceId,
        intent: PlaybackIntent,
    ) -> ExactMediaTransportOutcome {
        if self.playback_state() == crate::PlaybackState::Ended
            && intent == PlaybackIntent::StartPlaying
        {
            return match self.play() {
                Ok(()) => ExactMediaTransportOutcome::Applied { media_instance_id },
                Err(error) => ExactMediaTransportOutcome::Failed {
                    media_instance_id,
                    stage: ExactMediaTransportFailureStage::Play,
                    error,
                },
            };
        }
        let resume_intent = match intent {
            PlaybackIntent::StartPlaying => PlaybackResumeIntent::Play,
            PlaybackIntent::StartPaused => PlaybackResumeIntent::Pause,
        };
        if let Err(error) = self.start_exact_seek_to_beginning(resume_intent) {
            return ExactMediaTransportOutcome::Failed {
                media_instance_id,
                stage: ExactMediaTransportFailureStage::SeekToBeginning,
                error,
            };
        }
        ExactMediaTransportOutcome::Applied { media_instance_id }
    }

    /// Старый public seek сохраняет recoverable-error compatibility и поэтому возвращает `Ok`.
    /// Exact transport добавляет owner-side pre/postcondition, чтобы app получил typed failure.
    /// `resume_intent` — состояние, в которое seek-транзакция вернёт playback после commit-а.
    fn start_exact_seek_to_beginning(
        &mut self,
        resume_intent: PlaybackResumeIntent,
    ) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        if !self.snapshot.timeline.seekable {
            let reason = self
                .snapshot
                .timeline
                .not_seekable_reason
                .unwrap_or(media_core::TimelineNotSeekableReason::UnknownTimeline);
            return Err(PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Exact seek-to-zero impossible: timeline not seekable ({reason:?})"),
            ));
        }
        if !self.pipeline.has_demuxer() {
            return Err(PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Exact seek-to-zero is not possible: media pipeline is not open",
            ));
        }
        self.start_seek_with_resume_intent(
            SeekRequest::absolute(std::time::Duration::ZERO.into()),
            resume_intent,
        )?;
        if self.seek_runtime.seek_landing_active() || self.snapshot.timeline.seeking {
            return Ok(());
        }
        Err(PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            "Exact seek-to-zero not accepted by player seek transaction",
        ))
    }
}
