//! Serialized exact-instance restart и neutral Stop внутри player owner-а.

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
        if let Err(error) = self.start_exact_seek_to_beginning() {
            return ExactMediaTransportOutcome::PartiallyApplied {
                media_instance_id,
                completed_stage: ExactMediaTransportFailureStage::Pause,
                failed_stage: ExactMediaTransportFailureStage::SeekToBeginning,
                error,
            };
        }
        ExactMediaTransportOutcome::Applied { media_instance_id }
    }

    /// Seek выполняется до final intent, поэтому failed seek не маскируется как navigation.
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
        if let Err(error) = self.start_exact_seek_to_beginning() {
            return ExactMediaTransportOutcome::Failed {
                media_instance_id,
                stage: ExactMediaTransportFailureStage::SeekToBeginning,
                error,
            };
        }
        let final_state_result = match intent {
            PlaybackIntent::StartPlaying => self.play(),
            PlaybackIntent::StartPaused => self.pause(),
        };
        if let Err(error) = final_state_result {
            return ExactMediaTransportOutcome::PartiallyApplied {
                media_instance_id,
                completed_stage: ExactMediaTransportFailureStage::SeekToBeginning,
                failed_stage: match intent {
                    PlaybackIntent::StartPlaying => ExactMediaTransportFailureStage::Play,
                    PlaybackIntent::StartPaused => ExactMediaTransportFailureStage::Pause,
                },
                error,
            };
        }
        ExactMediaTransportOutcome::Applied { media_instance_id }
    }

    /// Старый public seek сохраняет recoverable-error compatibility и поэтому возвращает `Ok`.
    /// Exact transport добавляет owner-side pre/postcondition, чтобы app получил typed failure.
    fn start_exact_seek_to_beginning(&mut self) -> PlayerResult<()> {
        if !self.snapshot.timeline.seekable {
            let reason = self
                .snapshot
                .timeline
                .not_seekable_reason
                .unwrap_or(media_core::TimelineNotSeekableReason::UnknownTimeline);
            return Err(PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Exact seek-to-zero невозможен: timeline не seekable ({reason:?})"),
            ));
        }
        if !self.pipeline.has_demuxer() {
            return Err(PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Exact seek-to-zero невозможен: media pipeline не открыт",
            ));
        }
        self.seek(SeekRequest::absolute(std::time::Duration::ZERO.into()))?;
        if self.seek_runtime.seek_landing_active() || self.snapshot.timeline.seeking {
            return Ok(());
        }
        Err(PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            "Exact seek-to-zero не был принят player seek transaction",
        ))
    }
}
