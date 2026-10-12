//! B9 на стороне очереди: ручной отказ виден как событие, а оборвавшееся воспроизведение
//! переоткрывается с позиции обрыва тем же путём Play строки.
//!
//! Последовательности настоящие (controller + runtime): элемент выбирается, его открытие
//! падает, наблюдается то, что получит приложение.

use std::sync::Arc;
use std::time::Duration;

use player_core::PlaybackIntent;
use playlist_core::{
    CachedPlaylistMetadata, PlaylistItemDraft, PlaylistItemId, PlaylistMediaKind, SecretUrlLocator,
};

use super::*;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::controller::{ControllerPlayItemOutcome, PlaylistErrorBehavior};
use crate::playlist_runtime::skip_notice_regressions::{
    clean_eof_plans_next, non_zero, queue_with_a_playing_from, runtime_with,
};
use crate::playlist_runtime::{
    PlaylistTargetFailure, PlaylistTargetFailureSummary, QueueBlockReason,
};
use crate::state::{
    LocalOpenSlot, MediaRetryPort, MediaRetryTarget, ReopenPosition, RetryStart,
    execute_media_retry,
};

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

fn item_fault() -> PlaylistTargetFailure {
    PlaylistTargetFailure::ItemFault(PlaylistTargetFailureSummary::Specific(Arc::from(
        "File not found",
    )))
}

/// Пользователь выбрал B, B не открылся (файл пропал): очередь сообщает ровно один ручной
/// отказ именно этого элемента.
#[test]
fn manual_row_play_failure_is_published_once_for_the_center() {
    let (controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let mut runtime = runtime_with(controller);
    let RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall {
        install, ..
    }) = runtime.play_playlist_row(item_ids[1])
    else {
        panic!("Play Line B starts installing");
    };

    let continuation = runtime.report_playlist_target_failure_before_admission(
        request_id(80),
        install,
        item_fault(),
    );

    assert!(
        continuation.is_none(),
        "manual refusal queue does not continue"
    );
    assert_eq!(runtime.take_manual_open_failure(), Some(item_ids[1]));
    assert_eq!(
        runtime.take_manual_open_failure(),
        None,
        "event is given once"
    );
}

/// Ручной выбор без сети — тоже ручной отказ (центр покажет «Повторить»).
#[test]
fn manual_row_play_without_network_is_published_for_the_center() {
    let (controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let mut runtime = runtime_with(controller);
    let RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall {
        install, ..
    }) = runtime.play_playlist_row(item_ids[1])
    else {
        panic!("Play Line B starts installing");
    };

    runtime.report_playlist_target_failure_before_admission(
        request_id(81),
        install,
        PlaylistTargetFailure::QueueBlocked(QueueBlockReason::NoNetwork),
    );

    assert_eq!(runtime.take_manual_open_failure(), Some(item_ids[1]));
}

/// Шаг автоматического пропуска ручным отказом не считается: его итог показывает цепочка
/// пропусков, а ошибка в центре с «Повторить» выскочила бы поверх идущей очереди.
#[test]
fn automatic_skip_step_is_not_a_manual_failure() {
    let (mut controller, _item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);

    let continuation = runtime.report_playlist_target_failure_before_admission(
        request_id(82),
        b_install,
        item_fault(),
    );

    assert!(continuation.is_some(), "skip continues its turn");
    assert_eq!(runtime.take_manual_open_failure(), None);
}

/// Автоматическая остановка из-за сети тоже не ручной отказ (её показывает B8-итог).
#[test]
fn automatic_network_stop_is_not_a_manual_failure() {
    let (mut controller, _item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);

    runtime.report_playlist_target_failure_before_admission(
        request_id(83),
        b_install,
        PlaylistTargetFailure::QueueBlocked(QueueBlockReason::NoNetwork),
    );

    assert_eq!(runtime.take_manual_open_failure(), None);
}

/// Элемент играл и оборвался по сети: переоткрытие — новый install того же элемента с
/// позицией обрыва. Обычный Play строки на том же состоянии перезапустил бы мёртвое media
/// «с начала» без install, поэтому это поведение и проверяется как отличие.
#[test]
fn reopen_after_playback_failure_installs_same_item_at_failure_position() {
    let (controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let mut runtime = runtime_with(controller);

    let Some(RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall {
        install,
        ..
    })) = runtime.reopen_failed_active_item_from(Duration::from_secs(83))
    else {
        panic!("Reopening starts a new install");
    };

    assert_eq!(install.item_id, item_ids[0]);
    assert_eq!(install.playback_intent, PlaybackIntent::StartPlaying);
    assert_eq!(
        install.start_position,
        StartupPosition::RestoreExact(Duration::from_secs(83)),
        "the position of the cliff is applied as is, without rollback \"at the end - from the beginning\""
    );
    // Контроль: обычный Play той же строки не делает install, а рестартует мёртвое media.
    let RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::RestartActive { .. }) =
        runtime.play_playlist_row(item_ids[0])
    else {
        panic!("The usual play string - RestartActive");
    };
}

#[test]
fn reopen_without_known_position_starts_from_the_beginning() {
    let (controller, _item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let mut runtime = runtime_with(controller);

    let Some(RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall {
        install,
        ..
    })) = runtime.reopen_failed_active_item_from(Duration::ZERO)
    else {
        panic!("Reopening starts a new install");
    };

    assert_eq!(install.start_position, StartupPosition::KeepStart);
}

/// Нет очереди или ничего не играло: переоткрывать нечего, и это не паника.
#[test]
fn reopen_without_queue_or_active_media_has_nothing_to_open() {
    use crate::app_wake::{AppWakeOwner, AppWakePort};

    let mut not_loaded =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    assert!(
        not_loaded
            .reopen_failed_active_item_from(Duration::from_secs(5))
            .is_none()
    );

    let mut idle = runtime_with(crate::playlist_runtime::PlaylistController::new());
    assert!(
        idle.reopen_failed_active_item_from(Duration::from_secs(5))
            .is_none()
    );
}

/// Порт повтора поверх настоящей очереди: Play строк идёт в runtime, остальное не нужно.
struct RuntimeBackedPort<'a> {
    runtime: &'a mut PlaylistRuntime,
    started_items: Vec<PlaylistItemId>,
}

impl RuntimeBackedPort<'_> {
    fn record(&mut self, outcome: Option<RuntimeRowPlayOutcome>) -> RetryStart {
        match outcome {
            Some(RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall {
                install,
                ..
            })) => {
                self.started_items.push(install.item_id);
                RetryStart::Started
            }
            _ => RetryStart::NothingToOpen,
        }
    }
}

impl MediaRetryPort for RuntimeBackedPort<'_> {
    fn local_open_slot(&self) -> LocalOpenSlot {
        LocalOpenSlot::Free
    }
    fn report_still_opening(&mut self) {}
    fn clear_failure(&mut self) {}
    fn open_local_file(&mut self, _path: std::path::PathBuf) {
        panic!("In this scenario, the file does not open.");
    }
    fn open_web_url(&mut self, _url: &crate::external_open::request::DroppedWebUrl) {
        panic!("In this scenario, the link does not open.");
    }
    fn play_queue_item(&mut self, item_id: PlaylistItemId) -> RetryStart {
        let outcome = self.runtime.play_playlist_row(item_id);
        self.record(Some(outcome))
    }
    fn resume_blocked_queue(&mut self) -> RetryStart {
        let outcome = self.runtime.play_queue_blocked_item();
        self.record(outcome)
    }
    fn reopen_failed_playback(&mut self, position: ReopenPosition) -> RetryStart {
        let ReopenPosition::At(position) = position else {
            panic!("In this scenario, the position is known");
        };
        let outcome = self.runtime.reopen_failed_active_item_from(position);
        self.record(outcome)
    }
    fn report_nothing_to_retry(&mut self) {}
}

/// B8: очередь остановилась на B из-за сети; «Повторить» = Play на B, открывается именно он.
#[test]
fn retry_on_blocked_queue_opens_the_blocked_item() {
    let (mut controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);
    runtime.report_playlist_target_failure_before_admission(
        request_id(84),
        b_install,
        PlaylistTargetFailure::QueueBlocked(QueueBlockReason::NoNetwork),
    );

    let mut port = RuntimeBackedPort {
        runtime: &mut runtime,
        started_items: Vec::new(),
    };
    execute_media_retry(&mut port, MediaRetryTarget::ResumeBlockedQueue);

    assert_eq!(port.started_items, [item_ids[1]], "opens B, not A");
}

/// Блок уже снят (например, пользователь сам выбрал другой элемент): повторять нечего.
#[test]
fn retry_on_queue_that_is_no_longer_blocked_opens_nothing() {
    let (controller, _item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let mut runtime = runtime_with(controller);

    let mut port = RuntimeBackedPort {
        runtime: &mut runtime,
        started_items: Vec::new(),
    };
    execute_media_retry(&mut port, MediaRetryTarget::ResumeBlockedQueue);

    assert!(port.started_items.is_empty());
}

#[test]
fn retry_of_queue_item_and_failed_playback_open_the_expected_items() {
    let (controller, item_ids) = web_queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let mut runtime = runtime_with(controller);

    let mut port = RuntimeBackedPort {
        runtime: &mut runtime,
        started_items: Vec::new(),
    };
    execute_media_retry(&mut port, MediaRetryTarget::QueueItem(item_ids[2]));
    execute_media_retry(
        &mut port,
        MediaRetryTarget::ReopenFailedPlayback {
            position: ReopenPosition::At(Duration::from_secs(5)),
        },
    );

    // C выбран Play-ом строки; затем «упавшим» считается active — A (C ещё не установлен).
    assert_eq!(port.started_items, [item_ids[2], item_ids[0]]);
}
