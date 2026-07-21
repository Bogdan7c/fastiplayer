//! Focused S17M tests exact compound projection без Linux/D-Bus process dependency.

use std::num::NonZeroU64;
use std::path::PathBuf;

use player_core::{MediaInstanceId, PlayerSnapshot};
use playlist_core::{
    AddPlaylistEntriesOutcome, CachedPlaylistMetadata, LocalLocator, PlaylistCompoundGroupDraft,
    PlaylistEntryDraft, PlaylistEntryId, PlaylistItemDraft, PlaylistItemId, PlaylistLocator,
    PlaylistMediaKind, PlaylistQueue, RepeatMode,
};

use super::*;
use crate::playlist_runtime::compound_view::ToggleCompoundDisclosure;
use crate::playlist_runtime::controller::PlaylistController;
use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};
use crate::playlist_runtime::view::PlaylistStructuralRevision;

/// Согласованные identities одной three-part compound fixture.
struct CompoundFixture {
    controller: PlaylistController,
    compound_entry_id: PlaylistEntryId,
    part_item_ids: Vec<PlaylistItemId>,
}

/// Создаёт part draft с безопасным title и потенциально secret-bearing fallback.
fn part_draft(fallback_name: &str, title: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(fallback_name)),
        None,
        CachedPlaylistMetadata::new(fallback_name, PlaylistMediaKind::Audio)
            .with_title(Some(title.to_owned())),
    )
}

/// Строит controller с одной compound group и exact source-order parts.
fn fixture(group_title: Option<&str>) -> CompoundFixture {
    let root_locator = PlaylistLocator::Local(LocalLocator::Native(PathBuf::from(
        "https-example.invalid-private-group-token-must-not-leak",
    )));
    let mut group_metadata = CachedPlaylistMetadata::new(
        "https://example.invalid/private/group?token=must-not-leak",
        PlaylistMediaKind::Audio,
    );
    if let Some(group_title) = group_title {
        group_metadata = group_metadata.with_title(Some(group_title.to_owned()));
    }
    let group = PlaylistCompoundGroupDraft::new(
        root_locator,
        group_metadata,
        vec![
            part_draft("part-1?token=secret", "Exact part one"),
            part_draft("part-2?token=secret", "Exact part two"),
            part_draft("part-3?token=secret", "Exact part three"),
        ],
    )
    .expect("focused compound fixture is non-empty");
    let mut queue = PlaylistQueue::new();
    let AddPlaylistEntriesOutcome::Added(allocated) = queue
        .append_entries(vec![PlaylistEntryDraft::Compound(group)])
        .expect("append focused S17M fixture")
    else {
        panic!("non-empty S17M fixture must allocate identities");
    };
    let compound_entry_id = allocated
        .iter_entry_ids()
        .next()
        .expect("compound entry ID");
    let part_item_ids = allocated.iter_playable_item_ids().collect();
    let mut controller = PlaylistController::new();
    controller.queue = queue;
    CompoundFixture {
        controller,
        compound_entry_id,
        part_item_ids,
    }
}

/// Устанавливает согласованные queue current, active identity и player snapshot.
fn bind_part(
    controller: &mut PlaylistController,
    part_item_id: PlaylistItemId,
    identity_value: u64,
) -> PlayerSnapshot {
    controller
        .queue
        .set_traversal_current(part_item_id)
        .expect("fixture part is committed");
    let non_zero = NonZeroU64::new(identity_value).expect("fixture identity is non-zero");
    let media_instance_id = MediaInstanceId::from_non_zero(non_zero);
    controller.active_media = Some(ActiveMediaIdentity::installed(
        Some(part_item_id),
        ActiveMediaLineageId::from_non_zero(non_zero),
        media_instance_id,
        PlaylistBindingGeneration(7),
    ));
    PlayerSnapshot {
        media_instance_id: Some(media_instance_id),
        media_title: Some("player fallback must not replace cached part title".to_owned()),
        source_label: Some("https://example.invalid/stream?token=must-not-leak".to_owned()),
        ..PlayerSnapshot::default()
    }
}

#[test]
fn collapsed_and_expanded_ui_state_do_not_change_external_projection() {
    let mut fixture = fixture(Some("Bounded group"));
    let snapshot = bind_part(&mut fixture.controller, fixture.part_item_ids[1], 11);
    let collapsed =
        capture_for_controller(&fixture.controller, PlaylistBindingGeneration(7), &snapshot)
            .expect("collapsed projection");
    fixture
        .controller
        .toggle_compound_disclosure(ToggleCompoundDisclosure {
            compound_entry_id: fixture.compound_entry_id,
            structural_revision: PlaylistStructuralRevision::INITIAL,
        });
    let expanded =
        capture_for_controller(&fixture.controller, PlaylistBindingGeneration(7), &snapshot)
            .expect("expanded projection");

    assert!(collapsed.binding == expanded.binding);
    assert_eq!(collapsed.metadata, expanded.metadata);
    assert_eq!(expanded.active_part_item_id, Some(fixture.part_item_ids[1]));
}

#[test]
fn first_middle_and_last_part_publish_exact_title_and_position_context() {
    let mut fixture = fixture(Some("Three-part group"));
    for (index, expected_title) in ["Exact part one", "Exact part two", "Exact part three"]
        .into_iter()
        .enumerate()
    {
        let part_item_id = fixture.part_item_ids[index];
        let snapshot = bind_part(&mut fixture.controller, part_item_id, 20 + index as u64);
        let projection =
            capture_for_controller(&fixture.controller, PlaylistBindingGeneration(7), &snapshot)
                .expect("exact part projection");
        assert_eq!(projection.active_part_item_id, Some(part_item_id));
        assert_eq!(projection.metadata.title.as_deref(), Some(expected_title));
        assert_eq!(
            projection.metadata.collection_context.as_deref(),
            Some(format!("Three-part group · Part {}/3", index + 1).as_str())
        );
    }
}

#[test]
fn shuffle_repeat_and_repeated_projection_do_not_mutate_current_or_visit_history() {
    let mut fixture = fixture(Some("Traversal group"));
    let snapshot = bind_part(&mut fixture.controller, fixture.part_item_ids[1], 31);
    fixture
        .controller
        .queue
        .enable_shuffle()
        .expect("shuffle fixture can be enabled");
    fixture.controller.repeat_mode = RepeatMode::RepeatQueue;
    let current_before = fixture.controller.queue.traversal_current();
    let revisions_before = fixture.controller.queue.revision_snapshot();
    let shuffle_before = fixture.controller.queue.shuffle_traversal_snapshot();

    let first =
        capture_for_controller(&fixture.controller, PlaylistBindingGeneration(7), &snapshot)
            .expect("first projection");
    let second =
        capture_for_controller(&fixture.controller, PlaylistBindingGeneration(7), &snapshot)
            .expect("second projection");

    assert!(first.binding == second.binding);
    assert_eq!(fixture.controller.queue.traversal_current(), current_before);
    assert!(fixture.controller.queue.revision_snapshot() == revisions_before);
    assert_eq!(
        fixture.controller.queue.shuffle_traversal_snapshot(),
        shuffle_before
    );
}

#[test]
fn stale_removal_generation_instance_and_secret_fallback_are_rejected_or_redacted() {
    let mut fixture = fixture(None);
    let snapshot = bind_part(&mut fixture.controller, fixture.part_item_ids[0], 41);
    let projection =
        capture_for_controller(&fixture.controller, PlaylistBindingGeneration(7), &snapshot)
            .expect("valid projection before removal");
    assert_eq!(
        projection.metadata.collection_context.as_deref(),
        Some("Part 1/3")
    );
    assert!(!format!("{:?}", projection.metadata).contains("token="));
    assert!(
        capture_for_controller(&fixture.controller, PlaylistBindingGeneration(8), &snapshot,)
            .is_none()
    );
    let wrong_instance_snapshot = PlayerSnapshot {
        media_instance_id: Some(MediaInstanceId::from_non_zero(
            NonZeroU64::new(99).expect("non-zero fixture"),
        )),
        ..snapshot.clone()
    };
    assert!(
        capture_for_controller(
            &fixture.controller,
            PlaylistBindingGeneration(7),
            &wrong_instance_snapshot,
        )
        .is_none()
    );
    fixture.controller.queue.clear();
    assert!(
        capture_for_controller(&fixture.controller, PlaylistBindingGeneration(7), &snapshot,)
            .is_none()
    );
}
