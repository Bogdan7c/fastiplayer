//! A15-live: Play из пустого плеера запрашивает открытие элемента очереди.

use playlist_core::{
    CachedPlaylistMetadata, PlaylistItemDraft, PlaylistItemId, PlaylistMediaKind, SecretUrlLocator,
};

use super::*;
use crate::playlist_runtime::controller::{
    ControllerAppendOutcome, ControllerPlayItemOutcome, PlaylistController,
};
use crate::playlist_runtime::identity::TransportActionOrigin;
use crate::playlist_runtime::skip_notice_regressions::{
    active_item_id, controller_mut, install_planned, runtime_with,
};

fn local_drafts(names: &[&str]) -> Vec<PlaylistItemDraft> {
    names
        .iter()
        .map(|name| {
            PlaylistItemDraft::url(
                SecretUrlLocator::from_reopenable_url(format!("http://media.test/{name}.mp4"))
                    .expect("valid test URL"),
                CachedPlaylistMetadata::new(format!("clip {name}"), PlaylistMediaKind::Video),
            )
        })
        .collect()
}

/// Очередь из трёх файлов, в которой ещё ничего не открывалось.
fn untouched_queue() -> (PlaylistController, Vec<PlaylistItemId>) {
    let mut controller = PlaylistController::new();
    let ControllerAppendOutcome::Added { item_ids, .. } = controller
        .append(local_drafts(&["a", "b", "c"]))
        .expect("append a, b, c")
    else {
        panic!("очередь не пустая");
    };
    (controller, item_ids)
}

/// Основной сценарий: пустой плеер + непустая очередь без current + Play → install первого.
#[test]
fn play_without_current_requests_open_of_first_item() {
    let (controller, item_ids) = untouched_queue();
    let mut runtime = runtime_with(controller);

    let Some(RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall {
        install,
        ..
    })) = runtime.play_idle_queue_target()
    else {
        panic!("Play должен запросить открытие первого элемента");
    };

    assert_eq!(install.item_id, item_ids[0]);
    // До реальной установки media ни один элемент не считается играющим.
    assert_eq!(active_item_id(&runtime), None);
    install_planned(controller_mut(&mut runtime), install, 10);
    assert_eq!(active_item_id(&runtime), Some(item_ids[0]));
}

/// Пустая очередь и установленное media — этот путь не вмешивается в Play.
#[test]
fn play_path_is_absent_for_empty_queue_and_installed_media() {
    let mut empty_runtime = runtime_with(PlaylistController::new());
    assert!(empty_runtime.play_idle_queue_target().is_none());

    let (mut controller, item_ids) = untouched_queue();
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_ids[0], TransportActionOrigin::Ui)
    else {
        panic!("A стартует install");
    };
    install_planned(&mut controller, install, 10);
    let mut runtime = runtime_with(controller);
    assert!(runtime.play_idle_queue_target().is_none());
}
