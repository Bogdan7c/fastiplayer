//! Restore открывает собственную ссылку ролика коллекции и пропускает строки,
//! у которых отдельной ссылки нет (а не уходит на корень плейлиста).

use playlist_core::PlaylistItemDraft;

use super::*;
use crate::playlist_runtime::controller::ControllerAppendOutcome;
use crate::playlist_runtime::operational_open::entry_fixtures_tests::{
    COLLECTION_ROOT_URL, EntryIdentity, imported_entry_draft,
};

const LINKED_1: &str = "https://www.youtube.com/watch?v=linked1";
const LINKED_2: &str = "https://www.youtube.com/watch?v=linked2";

fn linked(url: &'static str) -> PlaylistItemDraft {
    imported_entry_draft("linked", &EntryIdentity::Original(url))
}

fn id_only() -> PlaylistItemDraft {
    imported_entry_draft(
        "id-only",
        &EntryIdentity::ExtractorOnly {
            extractor_key: "Youtube",
            extractor_id: "only-id",
        },
    )
}

/// Контроллер со строками `drafts`; current = строка с индексом `current_index`.
fn controller_restoring(
    drafts: Vec<PlaylistItemDraft>,
    current_index: usize,
) -> (PlaylistController, Vec<PlaylistItemId>) {
    let mut controller = PlaylistController::new();
    controller.set_error_behavior(PlaylistErrorBehavior::Skip);
    let ControllerAppendOutcome::Added { item_ids, .. } =
        controller.append(drafts).expect("append fixture")
    else {
        panic!("fixture is non-empty");
    };
    controller
        .queue
        .set_traversal_current(item_ids[current_index])
        .expect("persisted current fixture");
    (controller, item_ids)
}

fn url_of(locator: &playlist_core::PlaylistLocator) -> &str {
    locator
        .as_secret_url()
        .expect("URL locator")
        .expose_secret_for_open()
}

#[test]
fn restored_current_opens_entry_url_but_keeps_row_identity_for_resume() {
    let (mut controller, item_ids) = controller_restoring(vec![linked(LINKED_1)], 0);

    let target = controller.startup_restored_current().expect("target");

    assert_eq!(target.item_id(), item_ids[0]);
    assert_eq!(url_of(&target.open_locator), LINKED_1);
    // Ключ resume-checkpoint остаётся прежней identity строки.
    assert_eq!(url_of(&target.locator), COLLECTION_ROOT_URL);
}

#[test]
fn restored_current_without_own_url_is_skipped_to_next_openable_row() {
    let (mut controller, item_ids) =
        controller_restoring(vec![id_only(), id_only(), linked(LINKED_2)], 0);

    let target = controller
        .startup_restored_current()
        .expect("skip chain reaches the linked row");

    assert_eq!(target.item_id(), item_ids[2]);
    assert_eq!(url_of(&target.open_locator), LINKED_2);
}

#[test]
fn queue_without_any_openable_row_stops_instead_of_opening_the_playlist() {
    let (mut controller, _item_ids) = controller_restoring(vec![id_only(), id_only()], 0);

    assert!(controller.startup_restored_current().is_none());
}

#[test]
fn failure_chain_skips_rows_without_own_url() {
    let (mut controller, item_ids) =
        controller_restoring(vec![linked(LINKED_1), id_only(), linked(LINKED_2)], 0);
    let first = controller.startup_restored_current().expect("first target");
    assert_eq!(first.item_id(), item_ids[0]);

    let outcome = controller.report_startup_restore_failure(first, Arc::from("не открылось"));

    let StartupRestoreFailureOutcome::OpenItem { target } = outcome else {
        panic!("skip policy continues to the next openable row");
    };
    assert_eq!(target.item_id(), item_ids[2]);
    assert_eq!(url_of(&target.open_locator), LINKED_2);
}
