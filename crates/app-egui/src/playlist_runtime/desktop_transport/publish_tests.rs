//! Тесты публикации desktop snapshot-а: позиция не раздувает revision и не пересобирает metadata.

use std::num::NonZeroU64;

use player_core::{MediaInstanceId, PlaybackState, PlayerSnapshot};

use super::*;
use crate::app_wake::AppWakeOwner;
use crate::playlist_runtime::PlaylistRuntimeLifecycle;
use crate::playlist_runtime::controller::PlaylistController;
use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};

fn owner() -> DesktopTransportOwner {
    DesktopTransportOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime))
}

/// Runtime с bound lifecycle и пустым controller-ом.
fn bound_runtime() -> PlaylistRuntime {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.bind_resumed_app_state().expect("bind app state");
    runtime.controller.install(PlaylistController::new());
    runtime
}

/// Объявляет внешнее media (без playlist item) активным и возвращает его instance id.
fn install_external_media(runtime: &mut PlaylistRuntime, identity_value: u64) -> MediaInstanceId {
    let binding_generation = match runtime.lifecycle {
        PlaylistRuntimeLifecycle::Bound(binding) => binding.binding_generation(),
        _ => panic!("fixture runtime must be bound"),
    };
    let non_zero = NonZeroU64::new(identity_value).expect("non-zero identity");
    let media_instance_id = MediaInstanceId::from_non_zero(non_zero);
    runtime
        .controller
        .as_mut()
        .expect("controller")
        .active_media = Some(ActiveMediaIdentity::installed(
        None,
        ActiveMediaLineageId::from_non_zero(non_zero),
        media_instance_id,
        binding_generation,
    ));
    media_instance_id
}

/// Player snapshot, согласованный с установленным media.
fn player_snapshot(media_instance_id: MediaInstanceId, position_secs: u64) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot {
        media_instance_id: Some(media_instance_id),
        media_title: Some("Exact title".to_owned()),
        source_label: Some("label".to_owned()),
        volume: 1.0,
        playback_state: PlaybackState::Playing,
        ..PlayerSnapshot::default()
    };
    snapshot.timeline.current_position = MediaTime::from_secs(position_secs);
    snapshot
}

/// Адрес буфера title: меняется, если строку пересобрали или склонировали.
fn title_buffer_address(owner: &DesktopTransportOwner) -> usize {
    owner
        .last_snapshot
        .metadata
        .title
        .as_ref()
        .map(|title| title.as_ptr() as usize)
        .expect("title is published")
}

#[test]
fn position_only_changes_keep_revision_and_metadata_buffers() {
    let mut owner = owner();
    let mut runtime = bound_runtime();
    let media = install_external_media(&mut runtime, 11);

    // Старт за порогом «Previous = перезапуск»: иначе can_go_previous честно
    // изменится при пересечении порога и даст отдельную полную публикацию.
    owner.publish_from_player(&player_snapshot(media, 10), &mut runtime);
    let revision_after_bind = owner.revision;
    let title_address = title_buffer_address(&owner);
    assert!(revision_after_bind > 0);

    for second in 11..=110 {
        owner.publish_from_player(&player_snapshot(media, second), &mut runtime);
    }

    assert_eq!(owner.revision, revision_after_bind);
    assert_eq!(
        owner.last_snapshot.revision,
        DesktopSnapshotRevision::new(revision_after_bind)
    );
    assert_eq!(owner.last_snapshot.position, MediaTime::from_secs(110));
    assert_eq!(
        title_buffer_address(&owner),
        title_address,
        "metadata must not be rebuilt or cloned for position-only updates"
    );
}

#[test]
fn crossing_previous_restart_threshold_is_one_capability_publication() {
    let mut owner = owner();
    let mut runtime = bound_runtime();
    let media = install_external_media(&mut runtime, 12);
    owner.publish_from_player(&player_snapshot(media, 0), &mut runtime);
    let revision_before = owner.revision;
    assert!(!owner.last_snapshot.capabilities.can_go_previous);

    owner.publish_from_player(&player_snapshot(media, 20), &mut runtime);
    owner.publish_from_player(&player_snapshot(media, 21), &mut runtime);

    assert_eq!(owner.revision, revision_before + 1);
    assert!(owner.last_snapshot.capabilities.can_go_previous);
}

#[test]
fn each_non_position_change_is_exactly_one_full_publication() {
    let mut owner = owner();
    let mut runtime = bound_runtime();
    let media = install_external_media(&mut runtime, 21);
    owner.publish_from_player(&player_snapshot(media, 5), &mut runtime);

    // Title.
    let mut changed = player_snapshot(media, 6);
    changed.media_title = Some("Another title".to_owned());
    let before = owner.revision;
    owner.publish_from_player(&changed, &mut runtime);
    assert_eq!(owner.revision, before + 1);
    assert_eq!(
        owner.last_snapshot.metadata.title.as_deref(),
        Some("Another title")
    );

    // Playback state.
    let mut paused = changed.clone();
    paused.playback_state = PlaybackState::Paused;
    owner.publish_from_player(&paused, &mut runtime);
    assert_eq!(owner.revision, before + 2);
    assert_eq!(
        owner.last_snapshot.playback_status,
        DesktopPlaybackStatus::Paused
    );

    // Volume.
    let mut quieter = paused.clone();
    quieter.volume = 0.5;
    owner.publish_from_player(&quieter, &mut runtime);
    assert_eq!(owner.revision, before + 3);
    assert_eq!(
        owner.last_snapshot.volume,
        EffectiveVolume::from_player(0.5).expect("valid volume")
    );

    // Seekable capability.
    let mut seekable = quieter.clone();
    seekable.timeline.seekable = true;
    owner.publish_from_player(&seekable, &mut runtime);
    assert_eq!(owner.revision, before + 4);
    assert!(owner.last_snapshot.capabilities.can_seek);

    // Повтор без изменений (только позиция) больше ничего не публикует.
    seekable.timeline.current_position = MediaTime::from_secs(60);
    owner.publish_from_player(&seekable, &mut runtime);
    assert_eq!(owner.revision, before + 4);
}

#[test]
fn new_media_instance_bumps_control_revision_with_one_publication() {
    let mut owner = owner();
    let mut runtime = bound_runtime();
    let first = install_external_media(&mut runtime, 31);
    owner.publish_from_player(&player_snapshot(first, 1), &mut runtime);
    let revision_before = owner.revision;
    let control_before = owner.last_snapshot.control_revision;

    let second = install_external_media(&mut runtime, 32);
    owner.publish_from_player(&player_snapshot(second, 0), &mut runtime);

    assert_eq!(owner.revision, revision_before + 1);
    assert_ne!(owner.last_snapshot.control_revision, control_before);
    assert_eq!(owner.last_snapshot.position, MediaTime::ZERO);
}

#[test]
fn seeked_receipt_is_cleared_by_exactly_one_publication_then_position_only() {
    let mut owner = owner();
    let mut runtime = bound_runtime();
    let media = install_external_media(&mut runtime, 41);
    owner.publish_from_player(&player_snapshot(media, 1), &mut runtime);
    let request_id = TimelineSeekRequestId::new(NonZeroU64::new(5).expect("non-zero"));
    owner.publish_seeked(request_id, MediaTime::from_secs(30));
    let revision_with_seeked = owner.revision;
    assert!(owner.last_snapshot.seeked.is_some());

    // Первый снимок после seeked чистит receipt: одна полная публикация.
    owner.publish_from_player(&player_snapshot(media, 31), &mut runtime);
    assert_eq!(owner.revision, revision_with_seeked + 1);
    assert_eq!(owner.last_snapshot.seeked, None);

    // Дальше движение позиции revision не двигает.
    owner.publish_from_player(&player_snapshot(media, 32), &mut runtime);
    owner.publish_from_player(&player_snapshot(media, 33), &mut runtime);
    assert_eq!(owner.revision, revision_with_seeked + 1);
    assert_eq!(owner.last_snapshot.position, MediaTime::from_secs(33));
}

#[test]
fn detached_publication_still_bumps_revision_and_resets_position() {
    let mut owner = owner();
    let mut runtime = bound_runtime();
    let media = install_external_media(&mut runtime, 51);
    owner.publish_from_player(&player_snapshot(media, 9), &mut runtime);
    let revision_before = owner.revision;

    owner.publish_detached();

    assert_eq!(owner.revision, revision_before + 1);
    assert_eq!(owner.last_snapshot.position, MediaTime::ZERO);
    assert_eq!(owner.last_snapshot.control_revision, None);
}
