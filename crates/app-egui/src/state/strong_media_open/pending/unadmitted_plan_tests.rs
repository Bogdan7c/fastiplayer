//! Корень бага сессии 07: план очереди не должен теряться, если файл не подготовился до
//! controller admission (тогда controller не знает request ID и не может продолжить skip).

use std::path::PathBuf;

use playlist_core::{CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind};

use super::*;
use crate::media_open::{LocalOpenFailureReason, MediaPreparationFailureKind};
use crate::playlist_runtime::TransportActionOrigin;
use crate::playlist_runtime::{ControllerPlayItemOutcome, PlaylistController};

fn request_id() -> MediaOpenRequestId {
    MediaOpenRequestId::from_non_zero(NonZeroU64::new(77).expect("fixture request id is non-zero"))
}

/// Настоящий план controller-а для строки `b.mkv`.
fn planned_install() -> crate::playlist_runtime::PlannedPlaylistInstall {
    let mut controller = PlaylistController::new();
    controller
        .append(vec![PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from("b.mkv")),
            None,
            CachedPlaylistMetadata::new("b.mkv".to_owned(), PlaylistMediaKind::Video),
        )])
        .expect("append fixture row");
    let item_id = controller
        .queue()
        .iter_playable_ids()
        .next()
        .expect("fixture row committed");
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_id, TransportActionOrigin::Ui)
    else {
        panic!("строка стартует install");
    };
    install
}

fn planned_admission() -> Option<PendingStrongMediaAdmission> {
    Some(PendingStrongMediaAdmission::Playlist(
        PreparedPlaylistTarget::Planned {
            install: planned_install(),
            supersedes: None,
        },
    ))
}

fn preparation_failed(kind: MediaPreparationFailureKind) -> Box<StrongMediaOpenError> {
    Box::new(StrongMediaOpenError::Terminal(
        MediaOpenTerminalOutcome::PreparationFailed {
            request_id: request_id(),
            kind,
        },
    ))
}

/// Файл удалён: план возвращается вместе с ошибкой, причина сохраняется для бейджа.
#[test]
fn missing_file_before_admission_returns_plan_with_error() {
    let expected_item_id = planned_install().item_id;

    let poll = return_unadmitted_playlist_plan(
        preparation_failed(MediaPreparationFailureKind::LocalOpen(
            LocalOpenFailureReason::FileNotFound,
        )),
        planned_admission(),
    );

    let PlaylistStrongMediaOpenPoll::TargetFailedBeforeAdmission(unstaged) = poll else {
        panic!("план очереди должен вернуться владельцу");
    };
    assert_eq!(unstaged.install.item_id, expected_item_id);
    assert_eq!(
        unstaged.error.user_failure_reason(),
        Some(crate::media_open::MediaOpenUserFailureReason::Preparation(
            LocalOpenFailureReason::FileNotFound
        ))
    );
}

/// Отмена во время подготовки (Clear, новое открытие) не превращается в пропуск.
#[test]
fn cancelled_preparation_keeps_plain_failure_without_plan() {
    let poll = return_unadmitted_playlist_plan(
        preparation_failed(MediaPreparationFailureKind::Cancelled),
        planned_admission(),
    );

    assert!(matches!(
        poll,
        PlaylistStrongMediaOpenPoll::Strong(StrongMediaOpenPoll::Failed(_))
    ));
}

/// Отмена request-а целиком тоже не продолжает очередь.
#[test]
fn cancelled_request_keeps_plain_failure_without_plan() {
    let cancelled = Box::new(StrongMediaOpenError::Terminal(
        MediaOpenTerminalOutcome::Cancelled {
            request_id: request_id(),
            cause: MediaInstallCancellationCause::Superseded,
        },
    ));

    let poll = return_unadmitted_playlist_plan(cancelled, planned_admission());

    assert!(matches!(
        poll,
        PlaylistStrongMediaOpenPoll::Strong(StrongMediaOpenPoll::Failed(_))
    ));
}

/// План уже принят controller-ом (staging пройден) — возвращать нечего, ошибка обычная.
#[test]
fn already_admitted_plan_keeps_plain_failure() {
    let poll = return_unadmitted_playlist_plan(
        preparation_failed(MediaPreparationFailureKind::LocalOpen(
            LocalOpenFailureReason::FileNotFound,
        )),
        None,
    );

    assert!(matches!(
        poll,
        PlaylistStrongMediaOpenPoll::Strong(StrongMediaOpenPoll::Failed(_))
    ));
}
