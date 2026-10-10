//! B7: Undo Clear возвращает не только строки, но и воспроизведение (что играло, с той же
//! позиции, Play/Pause). Тесты идут через runtime-границы, как делает приложение: Clear с
//! позицией из player-а -> reset receipt -> Undo -> план открытия строки -> post-install позиция.

use player_core::{ExactMediaTransportAction, PlaybackIntent};

use super::current_change::{current_item_id, queue_item_ids};
use super::*;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::controller::{
    ControllerPlayItemOutcome, PlannedPlaylistInstall, PlaylistInstallRequest, StablePlaybackIntent,
};
use crate::playlist_runtime::identity::TransportActionOrigin;
use crate::playlist_runtime::{PlaybackProgressAtClear, StartupPosition};
use crate::state::{
    PostInstalledStartupPositionFacts, PostInstalledStartupPositionPlan,
    PreparedPositionRestoreStrategy, plan_post_installed_startup_position,
};

const POSITION_1_23: Duration = Duration::from_secs(83);

/// Играет (или стоит на паузе) элемент `index` из трёх; возвращает runtime и id строк.
fn runtime_playing(
    index: usize,
    intent: StablePlaybackIntent,
) -> (PlaylistRuntime, Vec<PlaylistItemId>) {
    let (mut runtime, ids) = runtime_with_items(3);
    install_active(&mut runtime, ids[index]);
    runtime
        .controller
        .record_stable_transport_intent(intent, TransportActionOrigin::Ui)
        .expect("intent revision available");
    (runtime, ids)
}

/// Достаёт план установки; любой другой исход Undo-запуска для этих тестов — ошибка.
fn start_install(playback: Option<ControllerPlayItemOutcome>) -> PlannedPlaylistInstall {
    match playback {
        Some(ControllerPlayItemOutcome::StartInstall { install, .. }) => install,
        Some(_) => panic!("Undo Clear должен планировать новое открытие строки"),
        None => panic!("Undo Clear должен вернуть воспроизведение"),
    }
}

/// Что post-install сделает с позицией: проходит реальную политику плана восстановления.
fn position_plan_after_install(
    install: &PlannedPlaylistInstall,
    duration: Duration,
) -> PostInstalledStartupPositionPlan {
    plan_post_installed_startup_position(
        install.start_position,
        PreparedPositionRestoreStrategy::SeekAfterInstall,
        PostInstalledStartupPositionFacts::static_seekable(Some(duration), Duration::ZERO),
    )
}

#[test]
fn clear_stops_playback_then_undo_reopens_same_item_at_same_position_playing() {
    let (mut runtime, ids) = runtime_playing(1, StablePlaybackIntent::Playing);
    let active = runtime.controller.active_media().expect("B играет");
    let now = Instant::now();

    let cleared =
        runtime.clear_playlist_observing_playback(now, PlaybackProgressAtClear::At(POSITION_1_23));

    // После Clear: очередь пуста, active сброшен, запрошен exact reset именно играющего media.
    assert!(matches!(cleared, RuntimeRemovalOutcome::Removed { .. }));
    assert!(queue_item_ids(&runtime).is_empty());
    assert!(runtime.controller.active_media().is_none());
    let reset = runtime
        .pending_media_reset_request()
        .expect("reset запрошен");
    assert_eq!(reset.media_instance_id, active.media_instance_id());
    assert_eq!(reset.action, ExactMediaTransportAction::ResetMedia);

    let undo = runtime.undo_last_removal_resuming_playback(now + Duration::from_secs(2));

    assert!(matches!(undo.outcome, RemovalUndoOutcome::Restored { .. }));
    assert_eq!(queue_item_ids(&runtime), ids);
    assert_eq!(current_item_id(&runtime), Some(ids[1]));
    let install = start_install(undo.playback);
    assert_eq!(install.item_id, ids[1]);
    assert_eq!(install.playback_intent, PlaybackIntent::StartPlaying);
    assert_eq!(
        install.start_position,
        StartupPosition::RestoreExact(POSITION_1_23)
    );
    assert_eq!(
        runtime.controller.stable_playback_intent(),
        StablePlaybackIntent::Playing
    );
    // Дальше post-install выполняет seek ровно на 01:23.
    assert_eq!(
        position_plan_after_install(&install, Duration::from_secs(600)),
        PostInstalledStartupPositionPlan::Restore {
            requested_position: POSITION_1_23,
            position: player_core::InstalledPositionRestore::SeekTo(POSITION_1_23),
        }
    );
}

#[test]
fn undo_of_clear_on_pause_reopens_with_pause_intent() {
    let (mut runtime, ids) = runtime_playing(1, StablePlaybackIntent::Paused);
    let now = Instant::now();
    runtime.clear_playlist_observing_playback(now, PlaybackProgressAtClear::At(POSITION_1_23));

    let undo = runtime.undo_last_removal_resuming_playback(now + Duration::from_secs(1));

    let install = start_install(undo.playback);
    assert_eq!(install.item_id, ids[1]);
    assert_eq!(install.playback_intent, PlaybackIntent::StartPaused);
    assert_eq!(
        install.start_position,
        StartupPosition::RestoreExact(POSITION_1_23)
    );
    assert_eq!(
        runtime.controller.stable_playback_intent(),
        StablePlaybackIntent::Paused
    );
}

#[test]
fn undo_clear_near_end_returns_to_exact_position_not_to_beginning() {
    // Решение B7: Undo возвращает «туда же», политика B2 (у конца -> с начала) не применяется.
    let (mut runtime, _ids) = runtime_playing(1, StablePlaybackIntent::Playing);
    let now = Instant::now();
    let three_seconds_before_end = Duration::from_secs(597);
    runtime.clear_playlist_observing_playback(
        now,
        PlaybackProgressAtClear::At(three_seconds_before_end),
    );

    let undo = runtime.undo_last_removal_resuming_playback(now + Duration::from_secs(1));

    let install = start_install(undo.playback);
    assert_eq!(
        position_plan_after_install(&install, Duration::from_secs(600)),
        PostInstalledStartupPositionPlan::Restore {
            requested_position: three_seconds_before_end,
            position: player_core::InstalledPositionRestore::SeekTo(three_seconds_before_end),
        }
    );
    // Контроль: тот же момент через resume из checkpoint (B2) начинается с начала.
    let checkpoint_plan = plan_post_installed_startup_position(
        StartupPosition::Restore(three_seconds_before_end),
        PreparedPositionRestoreStrategy::SeekAfterInstall,
        PostInstalledStartupPositionFacts::static_seekable(
            Some(Duration::from_secs(600)),
            Duration::ZERO,
        ),
    );
    assert_eq!(
        checkpoint_plan,
        PostInstalledStartupPositionPlan::Continue { warning: None }
    );
}

#[test]
fn clear_while_item_is_still_opening_reopens_that_item_from_start() {
    let (mut runtime, ids) = runtime_playing(0, StablePlaybackIntent::Playing);
    // Пользователь кликнул C: идёт открытие, A ещё играет.
    let ControllerPlayItemOutcome::StartInstall { install, .. } = runtime
        .controller
        .play_item(ids[2], TransportActionOrigin::Ui)
    else {
        panic!("клик по другой строке начинает установку");
    };
    runtime
        .controller
        .accept_install_request(PlaylistInstallRequest {
            request_id: MediaOpenRequestId::from_non_zero(non_zero(7_001)),
            player_request_id: MediaInstallRequestId::from_non_zero(non_zero(7_002)),
            target_item_id: Some(install.item_id),
            origin: install.pending_origin,
            intent_revision: install.intent_revision,
            expected_queue_revision: install.expected_queue_revision,
            mutation: install.mutation,
        })
        .expect("запрос принят");
    let now = Instant::now();

    runtime.clear_playlist_observing_playback(now, PlaybackProgressAtClear::At(POSITION_1_23));
    let undo = runtime.undo_last_removal_resuming_playback(now + Duration::from_secs(1));

    // Открывалась C, позиция A к ней не относится.
    let reopened = start_install(undo.playback);
    assert_eq!(reopened.item_id, ids[2]);
    assert_eq!(reopened.start_position, StartupPosition::KeepStart);
}

#[test]
fn undo_of_clear_when_nothing_was_playing_restores_rows_only() {
    let (mut runtime, ids) = runtime_with_items(3);
    let now = Instant::now();
    runtime.clear_playlist_observing_playback(now, PlaybackProgressAtClear::NotPlaying);

    let undo = runtime.undo_last_removal_resuming_playback(now + Duration::from_secs(1));

    assert!(matches!(undo.outcome, RemovalUndoOutcome::Restored { .. }));
    assert!(undo.playback.is_none());
    assert_eq!(queue_item_ids(&runtime), ids);
    assert!(runtime.controller.active_media().is_none());
}

#[test]
fn undo_of_clear_after_user_stop_does_not_start_playback() {
    let (mut runtime, _ids) = runtime_playing(1, StablePlaybackIntent::Playing);
    // Пользователь нажал Stop: очередь остановлена, ничего возобновлять не надо.
    runtime.controller.transport_disposition =
        crate::playlist_runtime::controller::AppTransportDisposition::Stopped;
    let now = Instant::now();
    runtime.clear_playlist_observing_playback(now, PlaybackProgressAtClear::At(POSITION_1_23));

    let undo = runtime.undo_last_removal_resuming_playback(now + Duration::from_secs(1));

    assert!(matches!(undo.outcome, RemovalUndoOutcome::Restored { .. }));
    assert!(undo.playback.is_none());
}

#[test]
fn undo_clear_is_unavailable_after_opening_another_file() {
    let (mut runtime, ids) = runtime_playing(1, StablePlaybackIntent::Playing);
    let now = Instant::now();
    runtime.clear_playlist_observing_playback(now, PlaybackProgressAtClear::At(POSITION_1_23));
    // Пользователь открыл другой файл (новая lineage, боевой путь регистрации Installed).
    let binding = runtime.bind_resumed_app_state().expect("binding");
    runtime
        .register_successful_strong_install(
            MediaOpenRequestId::from_non_zero(non_zero(8_001)),
            MediaInstallRequestId::from_non_zero(non_zero(8_002)),
            MediaInstanceId::from_non_zero(non_zero(8_003)),
            binding,
            crate::state::ActiveMediaSource::LocalFile("other.mp4".into()),
            PlaybackIntent::StartPlaying,
        )
        .expect("внешний файл установлен");

    assert!(
        runtime
            .removal_undo_status(now + Duration::from_secs(1))
            .is_none()
    );
    let undo = runtime.undo_last_removal_resuming_playback(now + Duration::from_secs(1));

    assert_eq!(undo.outcome, RemovalUndoOutcome::Unavailable);
    assert!(undo.playback.is_none());
    // Очередь осталась пустой, чужой файл продолжает играть.
    assert!(queue_item_ids(&runtime).is_empty());
    assert!(runtime.controller.active_media().is_some());
    let _ = ids;
}

#[test]
fn undo_clear_after_eight_seconds_is_expired_and_opens_nothing() {
    let (mut runtime, _ids) = runtime_playing(1, StablePlaybackIntent::Playing);
    let now = Instant::now();
    runtime.clear_playlist_observing_playback(now, PlaybackProgressAtClear::At(POSITION_1_23));

    let undo = runtime.undo_last_removal_resuming_playback(now + Duration::from_secs(8));

    assert_eq!(undo.outcome, RemovalUndoOutcome::Expired);
    assert!(undo.playback.is_none());
    assert!(queue_item_ids(&runtime).is_empty());
}

#[test]
fn undo_of_ordinary_removal_never_plans_playback() {
    let (mut runtime, ids) = runtime_playing(1, StablePlaybackIntent::Playing);
    let now = Instant::now();
    runtime.remove_playlist_item(ids[0], now);

    let undo = runtime.undo_last_removal_resuming_playback(now + Duration::from_secs(1));

    assert!(matches!(undo.outcome, RemovalUndoOutcome::Restored { .. }));
    assert!(undo.playback.is_none());
}
