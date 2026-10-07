//! Выбор locator-а для открытия строки очереди: ролик коллекции открывается по своей
//! ссылке, всё остальное — по `item.locator()`; extractor-only — типизированный отказ.

use std::path::PathBuf;

use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistLocator, PlaylistMediaKind,
    SecretUrlLocator,
};

use super::entry_fixtures_tests::{COLLECTION_ROOT_URL, EntryIdentity, imported_entry_draft};
use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::controller::{
    ControllerAppendOutcome, ControllerPlayItemOutcome, PlaylistController,
};
use crate::playlist_runtime::identity::TransportActionOrigin;

const ENTRY_URL: &str = "https://www.youtube.com/watch?v=entry1";

fn url_text(locator: &PlaylistLocator) -> &str {
    locator
        .as_secret_url()
        .expect("URL locator")
        .expose_secret_for_open()
}

/// Runtime с очередью из переданных черновиков и планом Play для каждой строки.
fn runtime_with(
    drafts: Vec<PlaylistItemDraft>,
) -> (PlaylistRuntime, Vec<playlist_core::PlaylistItemId>) {
    let mut controller = PlaylistController::new();
    let ControllerAppendOutcome::Added { item_ids, .. } =
        controller.append(drafts).expect("append fixture")
    else {
        panic!("fixture is non-empty");
    };
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.controller.install(controller);
    (runtime, item_ids)
}

/// Row Play строки -> план установки (тот же вход, что у UI).
fn planned_install_for_row(
    runtime: &mut PlaylistRuntime,
    item_id: playlist_core::PlaylistItemId,
) -> PlannedPlaylistInstall {
    let ControllerPlayItemOutcome::StartInstall { install, .. } = runtime
        .controller
        .as_mut()
        .expect("controller")
        .play_item(item_id, TransportActionOrigin::Ui)
    else {
        panic!("idle row Play starts install");
    };
    install
}

#[test]
fn original_and_webpage_identity_open_the_entry_not_the_collection_root() {
    for identity in [
        EntryIdentity::Original(ENTRY_URL),
        EntryIdentity::Webpage(ENTRY_URL),
    ] {
        let (runtime, item_ids) = runtime_with(vec![imported_entry_draft("entry", &identity)]);
        let item = runtime
            .controller
            .as_ref()
            .expect("controller")
            .queue()
            .item(item_ids[0])
            .expect("row");
        // Корень коллекции по-прежнему identity строки: persisted shape не менялся.
        assert_eq!(url_text(item.locator()), COLLECTION_ROOT_URL);

        let open = operational_open_locator(item).expect("entry has its own identity");

        assert_eq!(url_text(&open), ENTRY_URL);
    }
}

#[test]
fn extractor_only_identity_is_refused_and_never_falls_back_to_root() {
    let (runtime, item_ids) = runtime_with(vec![imported_entry_draft(
        "entry",
        &EntryIdentity::ExtractorOnly {
            extractor_key: "Youtube",
            extractor_id: "entry1",
        },
    )]);
    let item = runtime
        .controller
        .as_ref()
        .expect("controller")
        .queue()
        .item(item_ids[0])
        .expect("row");

    let refusal = operational_open_locator(item).expect_err("no URL form");

    assert_eq!(
        refusal,
        OperationalOpenLocatorError::Service(
            YtDlpDurableReopenDecodeError::ExtractorIdentityWithoutUrl
        )
    );
    let text = refusal.to_string();
    assert!(text.contains("собственная ссылка"), "{text}");
    assert!(!text.contains("entry1") && !text.contains("Youtube"));
}

#[test]
fn plain_url_and_local_rows_keep_item_locator() {
    let plain_url = SecretUrlLocator::from_reopenable_url("https://example.test/video.mp4?t=1")
        .expect("plain URL");
    let local = LocalLocator::Native(PathBuf::from("/media/local.mkv"));
    let (runtime, item_ids) = runtime_with(vec![
        PlaylistItemDraft::url(
            plain_url,
            CachedPlaylistMetadata::new("plain", PlaylistMediaKind::Video),
        ),
        PlaylistItemDraft::local(
            local.clone(),
            None,
            CachedPlaylistMetadata::new("local", PlaylistMediaKind::Video),
        ),
    ]);
    let queue = runtime.controller.as_ref().expect("controller").queue();

    let plain_open =
        operational_open_locator(queue.item(item_ids[0]).expect("plain")).expect("plain");
    let local_open =
        operational_open_locator(queue.item(item_ids[1]).expect("local")).expect("local");

    assert_eq!(url_text(&plain_open), "https://example.test/video.mp4?t=1");
    assert_eq!(local_open, PlaylistLocator::Local(local));
}

#[test]
fn row_play_intent_uses_entry_url_and_refusal_has_human_summary() {
    let (mut runtime, item_ids) = runtime_with(vec![
        imported_entry_draft("ok", &EntryIdentity::Original(ENTRY_URL)),
        imported_entry_draft(
            "id-only",
            &EntryIdentity::ExtractorOnly {
                extractor_key: "Youtube",
                extractor_id: "entry2",
            },
        ),
    ]);

    let open_install = planned_install_for_row(&mut runtime, item_ids[0]);
    let intent = runtime
        .media_open_intent_for_planned_install(&open_install)
        .expect("entry intent");
    assert_eq!(url_text(intent.locator()), ENTRY_URL);
    assert!(
        runtime
            .operational_open_refusal_summary(&open_install)
            .is_none()
    );

    // Второй Row Play в том же runtime: сначала возвращаем первый в idle через новый runtime.
    let (mut runtime, item_ids) = runtime_with(vec![
        imported_entry_draft("ok", &EntryIdentity::Original(ENTRY_URL)),
        imported_entry_draft(
            "id-only",
            &EntryIdentity::ExtractorOnly {
                extractor_key: "Youtube",
                extractor_id: "entry2",
            },
        ),
    ]);
    let refused_install = planned_install_for_row(&mut runtime, item_ids[1]);
    let refusal = runtime
        .media_open_intent_for_planned_install(&refused_install)
        .err()
        .expect("extractor-only row must not produce an open intent");
    assert!(matches!(
        refusal,
        PlaylistMediaOpenGateError::OperationalLocatorRefused(_)
    ));
    let summary = runtime
        .operational_open_refusal_summary(&refused_install)
        .expect("human summary for the row badge");
    assert!(summary.contains("откройте плейлист по исходной ссылке заново"));
}
