//! Бейдж строки очереди при отказе player-а установить элемент (сессия 03).

use std::num::NonZeroU64;
use std::sync::Arc;

use super::*;
use crate::media_open::{MediaOpenTerminalOutcome, PlayerInstallFailureReason};
use crate::playlist_runtime::PlaylistTargetFailureSummary;

fn request_id() -> MediaOpenRequestId {
    MediaOpenRequestId::from_non_zero(NonZeroU64::MIN)
}

#[test]
fn player_install_failure_puts_specific_reason_on_queue_row() {
    let error = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PlayerFailed {
        request_id: request_id(),
        reason: PlayerInstallFailureReason::UnsupportedVideoFormat,
    });

    assert_eq!(
        playlist_target_failure_summary(&error),
        PlaylistTargetFailureSummary::Specific(Arc::from("Формат видео не поддерживается"))
    );
}

/// Ошибки без классификации сохраняют прежнее общее поведение строки.
#[test]
fn unclassified_failure_keeps_generic_row_summary() {
    assert_eq!(
        playlist_target_failure_summary(&StrongMediaOpenError::MissingTerminal),
        PlaylistTargetFailureSummary::Generic
    );
}
