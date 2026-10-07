//! Enrichment метаданных спрашивает у ролика, а не у коллекции, из которой он импортирован.

use playlist_core::PlaylistItemDraft;

use super::*;
use crate::playlist_runtime::controller::{ControllerAppendOutcome, PlaylistController};
use crate::playlist_runtime::operational_open::entry_fixtures_tests::{
    COLLECTION_ROOT_URL, EntryIdentity, imported_entry_draft,
};

const ENTRY_URL: &str = "https://www.youtube.com/watch?v=entry1";

fn controller_with(drafts: Vec<PlaylistItemDraft>) -> (PlaylistController, Vec<PlaylistItemId>) {
    let mut controller = PlaylistController::new();
    let ControllerAppendOutcome::Added { item_ids, .. } =
        controller.append(drafts).expect("append fixture")
    else {
        panic!("fixture is non-empty");
    };
    (controller, item_ids)
}

#[test]
fn demand_requests_entry_url_and_guards_by_row_identity() {
    let (controller, item_ids) = controller_with(vec![imported_entry_draft(
        "entry",
        &EntryIdentity::Original(ENTRY_URL),
    )]);

    let demands = yt_dlp_metadata_demands(
        &controller,
        &item_ids,
        &fastiplayer_config::YtDlpConfig::default(),
    );

    assert_eq!(demands.len(), 1);
    assert_eq!(
        demands[0]
            .requested_locator()
            .expose_secret_for_persistence(),
        ENTRY_URL
    );
    // Stale-guard остаётся на identity строки (корень коллекции), а не на запрошенной ссылке.
    assert_eq!(
        demands[0]
            .expected_locator()
            .as_secret_url()
            .expect("row URL")
            .expose_secret_for_open(),
        COLLECTION_ROOT_URL
    );
}

#[test]
fn extractor_only_row_gets_no_demand_instead_of_resolving_the_whole_collection() {
    let (controller, item_ids) = controller_with(vec![imported_entry_draft(
        "entry",
        &EntryIdentity::ExtractorOnly {
            extractor_key: "Youtube",
            extractor_id: "entry1",
        },
    )]);

    let demands = yt_dlp_metadata_demands(
        &controller,
        &item_ids,
        &fastiplayer_config::YtDlpConfig::default(),
    );

    assert!(demands.is_empty());
}
