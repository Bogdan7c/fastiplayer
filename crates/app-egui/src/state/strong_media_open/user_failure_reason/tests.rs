//! Что видит пользователь по итогам strong open: причина, «молча» или внутренняя ошибка.

use std::path::Path;

use player_core::MediaInstallCancellationCause;

use super::*;
use crate::local_open_message::local_open_failure_message;
use crate::media_open::{LocalOpenFailureReason, MediaOpenRequestId, MediaPreparationFailureKind};

fn request_id() -> MediaOpenRequestId {
    MediaOpenRequestId::from_non_zero(std::num::NonZeroU64::MIN)
}

fn player_failed(reason: PlayerInstallFailureReason) -> StrongMediaOpenError {
    StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PlayerFailed {
        request_id: request_id(),
        reason,
    })
}

#[test]
fn local_preparation_terminal_exposes_user_reason() {
    let error = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PreparationFailed {
        request_id: request_id(),
        kind: MediaPreparationFailureKind::LocalOpen(LocalOpenFailureReason::FileNotFound),
    });

    assert_eq!(
        error.user_failure_reason(),
        Some(MediaOpenUserFailureReason::Preparation(
            LocalOpenFailureReason::FileNotFound
        ))
    );
}

#[test]
fn changed_source_terminal_maps_to_changed_during_open() {
    let error = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PreparationFailed {
        request_id: request_id(),
        kind: MediaPreparationFailureKind::LocalSourceChanged,
    });

    assert_eq!(
        error.user_failure_reason(),
        Some(MediaOpenUserFailureReason::Preparation(
            LocalOpenFailureReason::ChangedDuringOpen
        ))
    );
}

#[test]
fn player_failure_terminal_exposes_player_reason() {
    let error = player_failed(PlayerInstallFailureReason::NoSuitableVideoDecoder);

    assert_eq!(
        error.user_failure_reason(),
        Some(MediaOpenUserFailureReason::PlayerInstall(
            PlayerInstallFailureReason::NoSuitableVideoDecoder
        ))
    );
    assert_eq!(
        error.user_outcome(),
        StrongMediaOpenUserOutcome::Failed(MediaOpenUserFailureReason::PlayerInstall(
            PlayerInstallFailureReason::NoSuitableVideoDecoder
        ))
    );
}

#[test]
fn player_rejection_keeps_its_own_variant_and_reason() {
    let error = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PlayerRejected {
        request_id: request_id(),
        reason: PlayerInstallFailureReason::PlayerNotResponding,
    });

    assert_eq!(
        error.user_outcome(),
        StrongMediaOpenUserOutcome::Failed(MediaOpenUserFailureReason::PlayerInstall(
            PlayerInstallFailureReason::PlayerNotResponding
        ))
    );
    // Различие Rejected/Failed для pre-barrier логики не потерялось.
    assert!(error.is_proven_pre_barrier_failure());
}

#[test]
fn cancellation_and_busy_are_not_user_errors() {
    let cancelled = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::Cancelled {
        request_id: request_id(),
        cause: MediaInstallCancellationCause::Superseded,
    });
    let busy = StrongMediaOpenError::Start(MediaOpenStartError::Busy);

    // Отмена молчит, «занято» — отдельный исход для временного уведомления (сессия 04).
    assert_eq!(
        cancelled.user_outcome(),
        StrongMediaOpenUserOutcome::Cancelled
    );
    assert_eq!(busy.user_outcome(), StrongMediaOpenUserOutcome::Busy);
    assert_eq!(cancelled.user_failure_reason(), None);
    assert_eq!(busy.user_failure_reason(), None);
}

/// Неклассифицированная ошибка — честная «внутренняя ошибка», а не английский debug-дамп.
#[test]
fn unclassified_errors_become_internal_error_not_debug_dump() {
    let panicked_preparation =
        StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PreparationFailed {
            request_id: request_id(),
            kind: MediaPreparationFailureKind::WorkerPanicked,
        });
    let internal = StrongMediaOpenUserOutcome::Failed(MediaOpenUserFailureReason::PlayerInstall(
        PlayerInstallFailureReason::InternalError,
    ));

    for error in [
        panicked_preparation,
        StrongMediaOpenError::MissingTerminal,
        StrongMediaOpenError::Start(MediaOpenStartError::ShuttingDown),
    ] {
        assert_eq!(error.user_failure_reason(), None, "{error:?}");
        assert_eq!(error.user_outcome(), internal, "{error:?}");
    }
}

/// Сквозная проверка текста кнопки Open: имя файла, причина, никакого debug-а и «worker».
#[test]
fn open_button_text_for_each_player_reason_names_file_and_reason() {
    let path = Path::new("/home/private-parent-dir/clip.mkv");
    let cases = [
        (
            PlayerInstallFailureReason::UnsupportedVideoFormat,
            "Could not open “clip.mkv”: unsupported video format",
        ),
        (
            PlayerInstallFailureReason::UnsupportedAudioFormat,
            "Could not open “clip.mkv”: unsupported audio format",
        ),
        (
            PlayerInstallFailureReason::NoSuitableVideoDecoder,
            "Could not open “clip.mkv”: no suitable video decoder (select Automatic in decoder settings)",
        ),
        (
            PlayerInstallFailureReason::PreparationTimedOut,
            "Could not open “clip.mkv”: reading the file is taking too long",
        ),
        (
            PlayerInstallFailureReason::PlayerNotResponding,
            "Could not open “clip.mkv”: the player is not responding; try again",
        ),
        (
            PlayerInstallFailureReason::InternalError,
            "Could not open “clip.mkv”: internal player error",
        ),
    ];

    for (reason, expected_text) in cases {
        let StrongMediaOpenUserOutcome::Failed(user_reason) = player_failed(reason).user_outcome()
        else {
            panic!("Player failure must be an apparent error: {reason:?}");
        };
        let text = local_open_failure_message(path, user_reason);
        assert_eq!(text, expected_text);
        assert!(!text.contains("worker"), "{text}");
        assert!(!text.contains("private-parent-dir"), "{text}");
        assert!(!text.contains('{') && !text.contains("Player"), "{text}");
    }
}

/// Отказ подготовки web-ссылки больше не «внутренняя ошибка»: причина доходит до
/// бейджа строки и до сообщения окна (UX сессия 08).
#[test]
fn web_preparation_terminal_exposes_web_reason() {
    let reason = crate::media_open::WebOpenFailureReason::NotFound;
    let error = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PreparationFailed {
        request_id: request_id(),
        kind: MediaPreparationFailureKind::DirectOpen(reason),
    });

    assert_eq!(
        error.user_failure_reason(),
        Some(MediaOpenUserFailureReason::WebOpen(reason))
    );
    assert_eq!(
        error.user_outcome(),
        StrongMediaOpenUserOutcome::Failed(MediaOpenUserFailureReason::WebOpen(reason))
    );
}

#[test]
fn only_user_and_shutdown_cancellation_causes_are_silent_cancels() {
    let cancelled = |cause| {
        StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::Cancelled {
            request_id: request_id(),
            cause,
        })
    };
    for (cause, expected) in [
        (MediaInstallCancellationCause::UserCancelled, true),
        (MediaInstallCancellationCause::TransportStop, true),
        (MediaInstallCancellationCause::LifecycleShutdown, true),
        (MediaInstallCancellationCause::Superseded, false),
        (MediaInstallCancellationCause::StructuralInvalidation, false),
        (MediaInstallCancellationCause::LifecycleSuspended, false),
    ] {
        assert_eq!(
            cancelled(cause).is_cancelled_by_user_or_shutdown(),
            expected,
            "{cause:?}"
        );
    }
    // Ошибка, не являющаяся отменой, никогда не считается «тихой».
    assert!(
        !player_failed(PlayerInstallFailureReason::InternalError)
            .is_cancelled_by_user_or_shutdown()
    );
    assert!(!StrongMediaOpenError::MissingTerminal.is_cancelled_by_user_or_shutdown());
}
