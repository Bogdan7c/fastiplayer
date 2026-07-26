//! Same-lineage position phase queue-neutral media-install protocol-а.

use super::{MediaInstallPhase, MediaInstallProtocol, MediaInstallRequestId, MediaInstanceId};

/// Typed staging policy: ordinary opens не наследуют same-lineage position gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaInstallPositionPreparation {
    /// Startup/settings/new-lineage сохраняют прежний Ready → authorize protocol.
    NotRequired,
    /// Same-lineage candidate обязан подтвердить позицию exact старого instance-а.
    SameLineage {
        /// Authoritative old instance, который должен оставаться active до commit-а.
        expected_old_media_instance_id: MediaInstanceId,
    },
}

/// Exact owner command запуска position gate только после app final-owner validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrepareMediaInstallPosition {
    pub request_id: MediaInstallRequestId,
}

/// Внутренняя request-owned phase state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MediaInstallProtocolState {
    /// Worker dequeued command, но ordinary preparation ещё не завершена.
    Accepted,
    /// Same-lineage candidate ждёт явного запуска strict position gate.
    ReadyForPositionPreparation,
    /// Position gate запущен и может ждать worker receipt без блокировки owner-а.
    PositionPreparing,
    /// Все ordinary fallible stages завершены; matching authorization допустима.
    ReadyToCommit,
    /// Terminal record опубликован, дальнейшие controls являются duplicate/stale.
    Terminal,
}

impl MediaInstallProtocol {
    /// Публикует same-lineage preauthorization phase до настоящего ReadyToCommit.
    pub(crate) fn mark_ready_for_position_preparation(&mut self) {
        debug_assert_eq!(self.state, MediaInstallProtocolState::Accepted);
        self.port
            .publish_ready_to_commit(MediaInstallPhase::ReadyForPositionPreparation {
                request_id: self.request_id,
            });
        self.state = MediaInstallProtocolState::ReadyForPositionPreparation;
    }

    /// Атомарно принимает exact запуск position preparation.
    pub(crate) fn begin_position_preparation(&mut self) -> bool {
        if self.state != MediaInstallProtocolState::ReadyForPositionPreparation {
            return false;
        }
        self.state = MediaInstallProtocolState::PositionPreparing;
        true
    }
}
