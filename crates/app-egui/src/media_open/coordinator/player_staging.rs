use super::*;

pub(super) const fn same_lineage_position(
    expected_old_media_instance_id: player_core::MediaInstanceId,
) -> MediaOpenPositionPreparation {
    MediaOpenPositionPreparation::SameLineage {
        expected_old_media_instance_id,
    }
}

impl MediaOpenCoordinator {
    /// Передаёт prepared media exact player transaction-у; caller поставляет app resource port.
    pub(crate) fn stage_at_player(
        &mut self,
        request_id: MediaOpenRequestId,
        intent: MediaOpenInstallIntent,
        video_resource_port: MediaInstallVideoResourcePort,
    ) -> Result<MediaInstallRequestId, MediaOpenCommandError> {
        self.stage_at_player_with_position(
            request_id,
            intent,
            video_resource_port,
            MediaOpenPositionPreparation::NotRequired,
        )
    }

    pub(super) fn stage_at_player_with_position(
        &mut self,
        request_id: MediaOpenRequestId,
        intent: MediaOpenInstallIntent,
        video_resource_port: MediaInstallVideoResourcePort,
        position_preparation: MediaOpenPositionPreparation,
    ) -> Result<MediaInstallRequestId, MediaOpenCommandError> {
        self.matching_current(request_id)?;
        let player_port = self
            .player_port
            .as_ref()
            .ok_or(MediaOpenCommandError::MissingPlayerBinding)?
            .clone();
        let current = self.matching_current_mut(request_id)?;
        if current.phase != MediaOpenPhase::Prepared {
            return Err(MediaOpenCommandError::InvalidPhase {
                actual: current.phase,
            });
        }
        let prepared_open = current
            .prepared_open
            .take()
            .expect("Prepared phase must own prepared media");
        let player_request_id = MediaInstallRequestId::new_unique();
        let descriptor = prepared_open.descriptor;
        match player_port.stage(
            player_request_id,
            prepared_open.prepared_media,
            intent,
            video_resource_port,
            position_preparation,
        ) {
            Ok(receipt) => {
                current.phase = MediaOpenPhase::PlayerStaging;
                current.descriptor = Some(descriptor);
                current.player_request_id = Some(player_request_id);
                current.install_receipt = Some(receipt);
                current.same_lineage_position = match position_preparation {
                    MediaOpenPositionPreparation::NotRequired => {
                        SameLineagePositionPreparationPhase::NotRequired
                    }
                    MediaOpenPositionPreparation::SameLineage { .. } => {
                        SameLineagePositionPreparationPhase::WaitingForPlayerReady
                    }
                };
                Ok(player_request_id)
            }
            Err(rejection) => {
                current.phase = MediaOpenPhase::Failed;
                current.terminal = Some(MediaOpenTerminalOutcome::PlayerRejected {
                    request_id,
                    rejection,
                });
                Err(MediaOpenCommandError::PlayerDispatch(rejection))
            }
        }
    }

    /// Запускает same-lineage gate; completion приходит через install phase receipt.
    pub(crate) fn prepare_same_lineage_position(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<(), MediaOpenCommandError> {
        let player_port = self
            .player_port
            .as_ref()
            .ok_or(MediaOpenCommandError::MissingPlayerBinding)?
            .clone();
        let current = self.matching_current_mut(request_id)?;
        if current.phase != MediaOpenPhase::PlayerStaging
            || current.same_lineage_position
                != SameLineagePositionPreparationPhase::ReadyForPositionPreparation
        {
            return Err(MediaOpenCommandError::InvalidPhase {
                actual: current.phase,
            });
        }
        let player_request_id = current
            .player_request_id
            .expect("same-lineage staged request has player request id");
        player_port
            .prepare_position(player_request_id)
            .map_err(MediaOpenCommandError::PlayerDispatch)?;
        current.same_lineage_position = SameLineagePositionPreparationPhase::PreparationDispatched;
        Ok(())
    }

    /// Same-lineage dispatch не публикует Enqueued до authoritative owner acceptance.
    pub(crate) fn authorize_ready_same_lineage(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<(), MediaOpenCommandError> {
        let player_port = self
            .player_port
            .as_ref()
            .ok_or(MediaOpenCommandError::MissingPlayerBinding)?
            .clone();
        let current = self.matching_current_mut(request_id)?;
        if current.phase != MediaOpenPhase::ReadyToCommit
            || current.same_lineage_position != SameLineagePositionPreparationPhase::ReadyToCommit
        {
            return Err(MediaOpenCommandError::InvalidPhase {
                actual: current.phase,
            });
        }
        let player_request_id = current
            .player_request_id
            .expect("Ready request must have player request id");
        current.phase = MediaOpenPhase::AuthorizationDispatchPending;
        match player_port.authorize(player_request_id) {
            Ok(receipt) => {
                current.pending_control = Some(PendingControl::Authorization(receipt));
                Ok(())
            }
            Err(rejection) => {
                current.authorization_resolution = Some(
                    AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue { rejection },
                );
                current.phase = MediaOpenPhase::ReadyToCommit;
                Err(MediaOpenCommandError::PlayerDispatch(rejection))
            }
        }
    }

    pub(super) fn drain_player_staging(&mut self) -> bool {
        let Some(current) = self.current.as_mut() else {
            return false;
        };
        if current.phase != MediaOpenPhase::PlayerStaging {
            return false;
        }
        let receipt = current
            .install_receipt
            .as_ref()
            .expect("PlayerStaging must own install receipt");
        if let Some(completion) = receipt.take_completion() {
            current.phase = MediaOpenPhase::Failed;
            current.terminal = Some(MediaOpenTerminalOutcome::PlayerFailed {
                request_id: current.request_id,
                completion,
            });
            return true;
        }
        let Some(phase) = receipt.take_ready() else {
            return false;
        };
        let request_id = match phase {
            MediaInstallPhase::ReadyForPositionPreparation { request_id } => {
                if Some(request_id) != current.player_request_id {
                    self.publish_fatal(MediaOpenInvariantViolation::MismatchedPlayerRequest);
                    return true;
                }
                if current.same_lineage_position
                    != SameLineagePositionPreparationPhase::WaitingForPlayerReady
                {
                    self.publish_fatal(MediaOpenInvariantViolation::UnexpectedPlayerInstallPhase);
                    return true;
                }
                current.same_lineage_position =
                    SameLineagePositionPreparationPhase::ReadyForPositionPreparation;
                return true;
            }
            MediaInstallPhase::ReadyToCommit { request_id } => request_id,
        };
        if Some(request_id) != current.player_request_id {
            self.publish_fatal(MediaOpenInvariantViolation::MismatchedPlayerRequest);
            return true;
        }
        current.phase = MediaOpenPhase::ReadyToCommit;
        match current.same_lineage_position {
            SameLineagePositionPreparationPhase::PreparationDispatched => {
                current.same_lineage_position = SameLineagePositionPreparationPhase::ReadyToCommit;
            }
            SameLineagePositionPreparationPhase::NotRequired => {}
            _ => {
                self.publish_fatal(MediaOpenInvariantViolation::UnexpectedPlayerInstallPhase);
                return true;
            }
        }
        true
    }
}
