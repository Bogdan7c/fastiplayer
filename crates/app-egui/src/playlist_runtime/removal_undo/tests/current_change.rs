//! A9b: Undo удаления переживает смену текущего элемента (без shuffle, при удалении
//! неиграющих строк) и по-прежнему сбрасывается во всех остальных случаях.

use super::*;
use crate::state::ActiveMediaSource;

/// Как именно Installed автоперехода доходит до runtime.
#[derive(Clone, Copy)]
enum InstalledPath {
    /// Прямой `on_playlist_installed` (минимальный путь).
    Direct,
    /// Боевой путь приложения: `register_successful_strong_install`, который зовёт
    /// `state::strong_media_open` после каждого успешного открытия (в том числе автоперехода).
    StrongInstall,
}

/// Проводит полный автопереход Ended -> Installed и возвращает id нового текущего элемента.
/// `seed` разводит идентификаторы запроса, instance и binding между переходами одного теста.
fn advance_automatically(runtime: &mut PlaylistRuntime, seed: u64) -> PlaylistItemId {
    advance_automatically_via(runtime, seed, InstalledPath::Direct)
}

fn advance_automatically_via(
    runtime: &mut PlaylistRuntime,
    seed: u64,
    path: InstalledPath,
) -> PlaylistItemId {
    let active = runtime
        .controller
        .active_media()
        .expect("fixture has active media");
    let automatic = runtime.controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    let AutomaticLifecycleOutcome::OpenItem { install } = automatic else {
        panic!("Ended must select the next row");
    };
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(seed));
    let player_request_id = MediaInstallRequestId::from_non_zero(non_zero(seed + 1));
    let target_item_id = install.item_id;
    runtime
        .controller
        .accept_install_request(PlaylistInstallRequest {
            request_id,
            player_request_id,
            target_item_id: Some(target_item_id),
            origin: install.pending_origin,
            intent_revision: install.intent_revision,
            expected_queue_revision: install.expected_queue_revision,
            mutation: install.mutation,
        })
        .expect("automatic request accepted");
    assert!(matches!(
        runtime.controller.on_ready_to_commit(request_id),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    runtime
        .controller
        .begin_authorization_dispatch(request_id)
        .expect("dispatch begins");
    assert!(
        runtime
            .controller
            .resolve_authorization_dispatch(
                request_id,
                AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
            )
            .expect("enqueue resolution")
            .is_none()
    );
    match path {
        InstalledPath::Direct => {
            runtime
                .on_playlist_installed(
                    request_id,
                    player_request_id,
                    MediaInstanceId::from_non_zero(non_zero(seed + 2)),
                    PlaylistBindingGeneration(seed + 3),
                )
                .expect("Installed commits next lineage");
        }
        InstalledPath::StrongInstall => {
            let binding = runtime
                .bind_resumed_app_state()
                .expect("runtime accepts a binding");
            runtime
                .register_successful_strong_install(
                    request_id,
                    player_request_id,
                    MediaInstanceId::from_non_zero(non_zero(seed + 2)),
                    binding,
                    ActiveMediaSource::LocalFile("next.mp4".into()),
                    player_core::PlaybackIntent::StartPlaying,
                )
                .expect("strong install registers the next lineage");
        }
    }
    target_item_id
}

pub(super) fn queue_item_ids(runtime: &PlaylistRuntime) -> Vec<PlaylistItemId> {
    runtime.controller.queue.iter_playable_ids().collect()
}

pub(super) fn current_item_id(runtime: &PlaylistRuntime) -> Option<PlaylistItemId> {
    runtime
        .controller
        .queue
        .traversal_current()
        .map(|current| current.item_id())
}

#[test]
fn undo_after_auto_advance_returns_only_the_row_and_keeps_new_current_playing() {
    let (mut runtime, ids) = runtime_with_items(3);
    install_active(&mut runtime, ids[0]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[1], now);
    assert_eq!(advance_automatically(&mut runtime, 2_000), ids[2]);
    let playing_after_advance = runtime.controller.active_media();
    assert!(runtime.removal_undo_status(now).is_some());

    let outcome = runtime.undo_last_removal(now + Duration::from_secs(1));

    assert!(matches!(
        outcome,
        RemovalUndoOutcome::Restored {
            reattached_active: false,
            ..
        }
    ));
    assert_eq!(queue_item_ids(&runtime), ids);
    assert_eq!(current_item_id(&runtime), Some(ids[2]));
    // Играющее не перезапускалось: lineage, instance и binding те же.
    assert_eq!(runtime.controller.active_media(), playing_after_advance);
}

#[test]
fn undo_of_row_before_current_keeps_pointer_on_same_track_after_index_shift() {
    let (mut runtime, ids) = runtime_with_items(4);
    install_active(&mut runtime, ids[2]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[0], now);
    assert_eq!(advance_automatically(&mut runtime, 3_000), ids[3]);
    let index_before_undo = queue_item_ids(&runtime).iter().position(|id| *id == ids[3]);
    assert_eq!(index_before_undo, Some(2));

    let outcome = runtime.undo_last_removal(now + Duration::from_secs(1));

    assert!(matches!(outcome, RemovalUndoOutcome::Restored { .. }));
    assert_eq!(queue_item_ids(&runtime), ids);
    assert_eq!(current_item_id(&runtime), Some(ids[3]));
    // Индекс сдвинулся на единицу, указатель следует за треком по id.
    assert_eq!(
        queue_item_ids(&runtime)
            .iter()
            .position(|id| Some(*id) == current_item_id(&runtime)),
        Some(3)
    );
}

#[test]
fn undo_after_auto_advance_is_unavailable_when_shuffle_is_enabled() {
    let (mut runtime, ids) = runtime_with_items(4);
    install_active(&mut runtime, ids[0]);
    runtime
        .controller
        .queue
        .enable_shuffle()
        .expect("fixture shuffle");
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[1], now);
    advance_automatically(&mut runtime, 4_000);

    assert!(runtime.removal_undo_status(now).is_none());
    assert_eq!(
        runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Unavailable
    );
    assert!(!queue_item_ids(&runtime).contains(&ids[1]));
}

#[test]
fn undo_after_auto_advance_is_unavailable_when_playing_row_was_removed() {
    let (mut runtime, ids) = runtime_with_items(3);
    install_active(&mut runtime, ids[0]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[0], now);
    assert_eq!(advance_automatically(&mut runtime, 5_000), ids[1]);

    assert_eq!(
        runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Unavailable
    );
    assert_eq!(queue_item_ids(&runtime), vec![ids[1], ids[2]]);
    assert_eq!(current_item_id(&runtime), Some(ids[1]));
}

#[test]
fn undo_without_current_change_restores_row_and_pointer_as_before() {
    let (mut runtime, ids) = runtime_with_items(3);
    install_active(&mut runtime, ids[0]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[1], now);

    let outcome = runtime.undo_last_removal(now + Duration::from_secs(1));

    assert!(matches!(outcome, RemovalUndoOutcome::Restored { .. }));
    assert_eq!(queue_item_ids(&runtime), ids);
    assert_eq!(current_item_id(&runtime), Some(ids[0]));
}

#[test]
fn restored_row_takes_part_in_navigation_after_undo() {
    let (mut runtime, ids) = runtime_with_items(4);
    install_active(&mut runtime, ids[0]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[2], now);
    // Без строки ids[2] автопереход идёт A -> B.
    assert_eq!(advance_automatically(&mut runtime, 6_000), ids[1]);
    assert!(matches!(
        runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Restored { .. }
    ));

    // После Undo следующий за B трек обязан быть вернувшимся ids[2], а не ids[3].
    assert_eq!(advance_automatically(&mut runtime, 7_000), ids[2]);
}

#[test]
fn structural_change_after_auto_advance_still_invalidates_undo() {
    let (mut runtime, ids) = runtime_with_items(3);
    install_active(&mut runtime, ids[0]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[1], now);
    advance_automatically(&mut runtime, 8_000);
    runtime
        .controller
        .append(vec![draft(9)])
        .expect("structural change");

    assert_eq!(
        runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Invalidated
    );
}

/// A9c: боевой автопереход регистрируется через `register_successful_strong_install`;
/// этот путь раньше безусловно сбрасывал Undo при смене lineage.
#[test]
fn undo_survives_auto_advance_registered_through_strong_install() {
    let (mut runtime, ids) = runtime_with_items(3);
    install_active(&mut runtime, ids[0]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[1], now);
    assert_eq!(
        advance_automatically_via(&mut runtime, 8_000, InstalledPath::StrongInstall),
        ids[2]
    );
    let playing_after_advance = runtime.controller.active_media();
    assert!(runtime.removal_undo_status(now).is_some());

    let outcome = runtime.undo_last_removal(now + Duration::from_secs(1));

    assert!(matches!(outcome, RemovalUndoOutcome::Restored { .. }));
    assert_eq!(queue_item_ids(&runtime), ids);
    assert_eq!(current_item_id(&runtime), Some(ids[2]));
    assert_eq!(runtime.controller.active_media(), playing_after_advance);
}

/// A9c: row выше играющей, тот же боевой путь.
#[test]
fn undo_of_row_above_playing_survives_strong_install_auto_advance() {
    let (mut runtime, ids) = runtime_with_items(4);
    install_active(&mut runtime, ids[2]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[0], now);
    assert_eq!(
        advance_automatically_via(&mut runtime, 9_000, InstalledPath::StrongInstall),
        ids[3]
    );

    assert!(matches!(
        runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Restored { .. }
    ));
    assert_eq!(queue_item_ids(&runtime), ids);
    assert_eq!(current_item_id(&runtime), Some(ids[3]));
}

/// A9c: регрессии боевого пути: shuffle и удалённая играющая строка по-прежнему сбрасывают Undo.
#[test]
fn strong_install_auto_advance_still_drops_undo_with_shuffle() {
    let (mut runtime, ids) = runtime_with_items(4);
    install_active(&mut runtime, ids[0]);
    runtime
        .controller
        .queue
        .enable_shuffle()
        .expect("fixture shuffle");
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[1], now);
    advance_automatically_via(&mut runtime, 10_000, InstalledPath::StrongInstall);

    assert!(runtime.removal_undo_status(now).is_none());
}

#[test]
fn strong_install_auto_advance_still_drops_undo_when_playing_row_was_removed() {
    let (mut runtime, ids) = runtime_with_items(3);
    install_active(&mut runtime, ids[0]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[0], now);
    advance_automatically_via(&mut runtime, 11_000, InstalledPath::StrongInstall);

    assert!(runtime.removal_undo_status(now).is_none());
}
