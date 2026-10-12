//! Сквозные проверки B8: очередь web-ссылок встаёт на первом элементе без сети.
//!
//! Последовательность как в приложении: A играет → доиграл → controller планирует B →
//! подготовка B падает → отчёт об отказе уходит владельцу очереди. Проверяется
//! наблюдаемое: какой элемент стал active, запрашивался ли C, есть ли у B бейдж «битый»,
//! какое уведомление увидит пользователь и что делает Play после остановки.

use std::sync::Arc;

use playlist_core::{
    CachedPlaylistMetadata, PlaylistItemDraft, PlaylistItemId, PlaylistMediaKind, SecretUrlLocator,
};

use super::*;
use crate::media_open::WebOpenFailureReason;
use crate::playlist_runtime::controller::{ControllerPlayItemOutcome, PlaylistErrorBehavior};
use crate::playlist_runtime::row_interactions::RuntimeRowPlayOutcome;
use crate::playlist_runtime::skip_notice_regressions::{
    active_item_id, clean_eof_plans_next, controller_mut, install_planned, non_zero,
    queue_with_a_playing_from, runtime_with, user_visible_messages,
};
use crate::playlist_runtime::{AutomaticQueueNotice, PlaylistTargetFailureSummary};

const NO_NETWORK_TEXT: &str = "No network connection; queue stopped";

/// Очередь из трёх web-ссылок; первая уже играет.
fn web_queue_with_a_playing(
    behavior: PlaylistErrorBehavior,
) -> (
    crate::playlist_runtime::PlaylistController,
    Vec<PlaylistItemId>,
) {
    let drafts = ["a", "b", "c"]
        .iter()
        .map(|name| {
            PlaylistItemDraft::url(
                SecretUrlLocator::from_reopenable_url(format!("http://media.test/{name}.mp4"))
                    .expect("valid test URL"),
                CachedPlaylistMetadata::new(format!("clip {name}"), PlaylistMediaKind::Video),
            )
        })
        .collect();
    queue_with_a_playing_from(drafts, behavior)
}

fn request_id(seed: u64) -> MediaOpenRequestId {
    MediaOpenRequestId::from_non_zero(non_zero(seed))
}

fn item_badge(runtime: &mut PlaylistRuntime, item_id: PlaylistItemId) -> Option<String> {
    controller_mut(runtime)
        .runtime_errors
        .get(&item_id)
        .map(|runtime_error| runtime_error.safe_summary().to_owned())
}

/// Основной сценарий владельца: автопереход на B, у B нет сети → очередь стоит на B, C не
/// запрашивался, B не «битый», пользователь видит причину в центре.
#[test]
fn no_network_at_auto_advance_stops_queue_on_b_without_touching_c() {
    for behavior in [PlaylistErrorBehavior::Skip, PlaylistErrorBehavior::Stop] {
        let (mut controller, item_ids) = web_queue_with_a_playing(behavior);
        let b_install = clean_eof_plans_next(&mut controller);
        assert_eq!(b_install.item_id, item_ids[1]);
        let mut runtime = runtime_with(controller);

        let continuation = runtime.report_playlist_target_failure_before_admission(
            request_id(60),
            b_install,
            PlaylistTargetFailure::QueueBlocked(QueueBlockReason::NoNetwork),
        );

        assert!(continuation.is_none(), "C not requested ({behavior:?})");
        assert_eq!(active_item_id(&runtime), Some(item_ids[0]), "{behavior:?}");
        assert_eq!(item_badge(&mut runtime, item_ids[1]), None, "B not broken");
        assert_eq!(item_badge(&mut runtime, item_ids[2]), None, "C untouched.");
        let blocked = runtime
            .playlist_controller()
            .and_then(|controller| controller.queue_blocked_target())
            .expect("The line remembers where it stopped.");
        assert_eq!(blocked.item_id, item_ids[1]);
        assert_eq!(blocked.reason, QueueBlockReason::NoNetwork);

        let notices = runtime.drain_automatic_queue_notices();
        assert_eq!(
            notices,
            [AutomaticQueueNotice::StoppedOpenBlocked {
                reason: QueueBlockReason::NoNetwork
            }]
        );
        let (toasts, center_error) = user_visible_messages(&notices);
        assert!(toasts.is_empty());
        assert_eq!(center_error.as_deref(), Some(NO_NETWORK_TEXT));
    }
}

/// То же, когда отказ приходит по request ID после admission плана (путь `Failed`).
#[test]
fn no_network_after_admission_stops_queue_on_b() {
    let (mut controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let b_request = request_id(61);
    controller
        .accept_install_request(super::super::transport_execution::playlist_install_request(
            b_request,
            player_core::MediaInstallRequestId::from_non_zero(non_zero(1_061)),
            b_install,
        ))
        .expect("B admission");
    let mut runtime = runtime_with(controller);

    let continuation = runtime.report_playlist_target_failure(
        b_request,
        item_ids[1],
        PlaylistTargetFailure::QueueBlocked(QueueBlockReason::NoNetwork),
    );

    assert!(continuation.is_none());
    assert_eq!(active_item_id(&runtime), Some(item_ids[0]));
    assert_eq!(item_badge(&mut runtime, item_ids[1]), None);
    let (_toasts, center_error) = user_visible_messages(&runtime.drain_automatic_queue_notices());
    assert_eq!(center_error.as_deref(), Some(NO_NETWORK_TEXT));
}

/// Лимит запросов сайта (429) — тот же механизм со своей формулировкой.
#[test]
fn site_rate_limit_stops_queue_with_its_own_reason() {
    let (mut controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);

    let continuation = runtime.report_playlist_target_failure_before_admission(
        request_id(62),
        b_install,
        PlaylistTargetFailure::QueueBlocked(QueueBlockReason::SiteRateLimited),
    );

    assert!(continuation.is_none());
    assert_eq!(item_badge(&mut runtime, item_ids[1]), None);
    let (_toasts, center_error) = user_visible_messages(&runtime.drain_automatic_queue_notices());
    assert_eq!(
        center_error.as_deref(),
        Some("Website limited requests - queue stopped")
    );
}

/// Регрессия: «видео не найдено» (404/410/недоступно) — вина ссылки; skip идёт на C, B с бейджем.
#[test]
fn item_fault_on_web_item_still_skips_to_c_with_badge() {
    let (mut controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);

    let c_install = runtime
        .report_playlist_target_failure_before_admission(
            request_id(63),
            b_install,
            PlaylistTargetFailure::ItemFault(PlaylistTargetFailureSummary::Specific(Arc::from(
                "Page not found (Error 404)",
            ))),
        )
        .expect("Rejection of the link itself is omitted");
    assert_eq!(c_install.item_id, item_ids[2]);
    assert_eq!(
        item_badge(&mut runtime, item_ids[1]).as_deref(),
        Some("Page not found (Error 404)")
    );
    assert!(
        runtime
            .playlist_controller()
            .and_then(|controller| controller.queue_blocked_target())
            .is_none(),
        "Normal failure does not block the line."
    );

    install_planned(controller_mut(&mut runtime), c_install, 70);
    assert_eq!(active_item_id(&runtime), Some(item_ids[2]));
    let (toasts, center_error) = user_visible_messages(&runtime.drain_automatic_queue_notices());
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(center_error, None);
}

/// Play после возвращения сети открывает именно B (а не перезапускает A), и блокировка снята.
#[test]
fn play_after_block_resumes_from_blocked_item() {
    let (mut controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);
    let _ = runtime.report_playlist_target_failure_before_admission(
        request_id(64),
        b_install,
        PlaylistTargetFailure::QueueBlocked(QueueBlockReason::NoNetwork),
    );

    let Some(RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall {
        install,
        ..
    })) = runtime.play_queue_blocked_item()
    else {
        panic!("Play opens the stopped element after stopping");
    };
    assert_eq!(install.item_id, item_ids[1]);
    install_planned(controller_mut(&mut runtime), install, 80);

    assert_eq!(active_item_id(&runtime), Some(item_ids[1]));
    assert!(
        runtime.play_queue_blocked_item().is_none(),
        "After successful installation, the queue is no longer blocked"
    );
}

/// Без блокировки Play не перехватывается; удалённый элемент блокировку не воскрешает.
#[test]
fn play_resume_is_absent_without_block_or_after_item_removal() {
    let (controller, _item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let mut runtime = runtime_with(controller);
    assert!(runtime.play_queue_blocked_item().is_none());

    // Блокируем B и удаляем её из очереди.
    let (mut controller, item_ids_2) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut blocked_runtime = runtime_with(controller);
    let _ = blocked_runtime.report_playlist_target_failure_before_admission(
        request_id(65),
        b_install,
        PlaylistTargetFailure::QueueBlocked(QueueBlockReason::NoNetwork),
    );
    assert!(
        blocked_runtime
            .playlist_controller()
            .and_then(|controller| controller.queue_blocked_target())
            .is_some()
    );
    let removal = controller_mut(&mut blocked_runtime).remove_item(item_ids_2[1]);
    assert!(
        !matches!(
            removal,
            crate::playlist_runtime::controller::ControllerDestructiveRemovalOutcome::NotFound { .. }
        ),
        "B removed from the queue"
    );
    assert!(blocked_runtime.play_queue_blocked_item().is_none());
}

/// Пользователь сам выбрал другую строку: остановка снимается, Play больше не прыгает на B.
#[test]
fn manual_row_play_clears_the_block() {
    let (mut controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);
    let _ = runtime.report_playlist_target_failure_before_admission(
        request_id(66),
        b_install,
        PlaylistTargetFailure::QueueBlocked(QueueBlockReason::NoNetwork),
    );

    let _ = runtime.play_playlist_row(item_ids[2]);

    assert!(runtime.play_queue_blocked_item().is_none());
}

/// Ручной выбор на B без сети: D55 «ждём пользователя», без автоперехода и без бейджа.
#[test]
fn manual_navigation_without_network_does_not_stop_queue_or_mark_item() {
    let (mut controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let ControllerPlayItemOutcome::StartInstall { install, .. } = controller.play_item(
        item_ids[1],
        crate::playlist_runtime::identity::TransportActionOrigin::Ui,
    ) else {
        panic!("Manual Play B starts installing");
    };
    let mut runtime = runtime_with(controller);

    let continuation = runtime.report_playlist_target_failure_before_admission(
        request_id(67),
        install,
        PlaylistTargetFailure::QueueBlocked(QueueBlockReason::NoNetwork),
    );

    assert!(continuation.is_none());
    assert_eq!(item_badge(&mut runtime, item_ids[1]), None);
    assert!(runtime.drain_automatic_queue_notices().is_empty());
}

/// Решение «чья вина» принимает типизированная причина, а не текст.
#[test]
fn only_connection_and_rate_limit_block_the_queue() {
    use WebOpenFailureReason as Reason;
    assert_eq!(
        QueueBlockReason::from_web_open_failure(Reason::NoConnection),
        Some(QueueBlockReason::NoNetwork)
    );
    assert_eq!(
        QueueBlockReason::from_web_open_failure(Reason::RateLimited),
        Some(QueueBlockReason::SiteRateLimited)
    );
    for item_fault in [
        Reason::NotFound,
        Reason::Gone,
        Reason::SiteMediaUnavailable,
        Reason::SitePrivateMedia,
        Reason::SiteLoginRequired,
        Reason::AuthenticationRequired,
        Reason::AccessDenied,
        Reason::ServerError,
        Reason::TimedOut,
        Reason::ConnectionInterrupted,
        Reason::UnrecognizedFormat,
        Reason::Unclassified,
    ] {
        assert_eq!(
            QueueBlockReason::from_web_open_failure(item_fault),
            None,
            "{item_fault:?} remains a failure element"
        );
    }
}
