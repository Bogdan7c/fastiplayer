//! Бейдж строки очереди при отказе player-а установить элемент (сессия 03).

use std::num::NonZeroU64;
use std::sync::Arc;

use super::*;
use crate::media_open::{MediaOpenTerminalOutcome, PlayerInstallFailureReason};
use crate::playlist_runtime::{PlaylistTargetFailure, PlaylistTargetFailureSummary};

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

/// UX сессия 08: отказ подготовки web-элемента очереди даёт строке понятную причину
/// вместо общего «Не удалось подготовить…».
#[test]
fn web_preparation_failure_puts_specific_reason_on_queue_row() {
    let error = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PreparationFailed {
        request_id: request_id(),
        kind: crate::media_open::MediaPreparationFailureKind::ExtractorOpen(
            crate::media_open::WebOpenFailureReason::SiteGeoRestricted,
        ),
    });

    assert_eq!(
        playlist_target_failure_summary(&error),
        PlaylistTargetFailureSummary::Specific(Arc::from("Видео недоступно в вашем регионе"))
    );
}

fn preparation_failure(
    kind: crate::media_open::MediaPreparationFailureKind,
) -> StrongMediaOpenError {
    StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PreparationFailed {
        request_id: request_id(),
        kind,
    })
}

/// B8: нет сети и лимит запросов — обстоятельства, общие для соседей; очередь встаёт.
#[test]
fn no_connection_and_rate_limit_are_queue_blocks_for_every_web_opener() {
    use crate::media_open::MediaPreparationFailureKind as Kind;
    use crate::media_open::WebOpenFailureReason as Reason;
    use crate::playlist_runtime::QueueBlockReason;

    let openers: [fn(Reason) -> Kind; 6] = [
        Kind::DirectOpen,
        Kind::NativeHlsOpen,
        Kind::NativeDashOpen,
        Kind::NativeHdsOpen,
        Kind::NativeSmoothOpen,
        Kind::ExtractorOpen,
    ];
    for opener in openers {
        assert_eq!(
            target_failure_class::playlist_target_failure(&preparation_failure(opener(
                Reason::NoConnection
            ))),
            PlaylistTargetFailure::QueueBlocked(QueueBlockReason::NoNetwork)
        );
        assert_eq!(
            target_failure_class::playlist_target_failure(&preparation_failure(opener(
                Reason::RateLimited
            ))),
            PlaylistTargetFailure::QueueBlocked(QueueBlockReason::SiteRateLimited)
        );
    }
}

/// B8: отказ самой ссылки (404, нужен вход, видео недоступно) остаётся отказом элемента.
#[test]
fn link_specific_web_failures_stay_item_faults_with_row_badge() {
    use crate::media_open::MediaPreparationFailureKind as Kind;
    use crate::media_open::WebOpenFailureReason as Reason;

    for (reason, expected_badge) in [
        (
            Reason::NotFound,
            "Страница не найдена (ошибка 404) — проверьте ссылку",
        ),
        (
            Reason::SiteLoginRequired,
            "Сайт требует войти в аккаунт (возрастное ограничение или проверка)",
        ),
        (
            Reason::SiteMediaUnavailable,
            "Видео удалено или не существует",
        ),
    ] {
        let failure = target_failure_class::playlist_target_failure(&preparation_failure(
            Kind::DirectOpen(reason),
        ));
        let PlaylistTargetFailure::ItemFault(PlaylistTargetFailureSummary::Specific(badge)) =
            failure
        else {
            panic!("{reason:?} должен остаться отказом элемента: {failure:?}");
        };
        assert_eq!(&*badge, expected_badge);
    }
}

/// B8: локальный файл (даже на «недоступном сетевом диске») и отказ player-а — вина элемента.
#[test]
fn local_and_player_failures_are_never_queue_blocks() {
    use crate::media_open::{LocalOpenFailureReason, MediaPreparationFailureKind as Kind};

    let local = target_failure_class::playlist_target_failure(&preparation_failure(
        Kind::LocalOpen(LocalOpenFailureReason::FileNotFound),
    ));
    assert!(
        matches!(
            local,
            PlaylistTargetFailure::ItemFault(PlaylistTargetFailureSummary::Specific(_))
        ),
        "{local:?}"
    );
    let player = target_failure_class::playlist_target_failure(&StrongMediaOpenError::Terminal(
        MediaOpenTerminalOutcome::PlayerFailed {
            request_id: request_id(),
            reason: PlayerInstallFailureReason::UnsupportedVideoFormat,
        },
    ));
    assert!(
        matches!(player, PlaylistTargetFailure::ItemFault(_)),
        "{player:?}"
    );
    assert_eq!(
        target_failure_class::playlist_target_failure(&StrongMediaOpenError::MissingTerminal),
        PlaylistTargetFailure::ItemFault(PlaylistTargetFailureSummary::Generic)
    );
}
