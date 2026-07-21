//! Focused S17G tests для process-lifetime compound view model и typed actions.

use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use player_core::MediaInstanceId;
use playlist_core::{
    AddPlaylistEntriesOutcome, CachedPlaylistMetadata, LocalLocator, PlaylistCompoundGroupDraft,
    PlaylistEntryDraft, PlaylistEntryId, PlaylistItemDraft, PlaylistItemId, PlaylistLocator,
    PlaylistMediaKind, PlaylistQueue,
};

use super::*;
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::identity::ActiveMediaLineageId;
use crate::playlist_runtime::selection::{PlaylistSelectionState, UpdateSelection};

/// Stable fixture IDs позволяют тестам явно различать structural entries и playable parts.
struct CompoundFixture {
    queue: PlaylistQueue,
    entry_ids: Vec<PlaylistEntryId>,
    item_ids: Vec<PlaylistItemId>,
}

/// Создаёт ID-less local draft без filesystem I/O.
fn item_draft(name: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(name)),
        None,
        CachedPlaylistMetadata::new(name, PlaylistMediaKind::Audio),
    )
}

/// Создаёт compound draft с одной или несколькими ordered parts.
fn compound_draft(root_name: &str, part_names: &[&str]) -> PlaylistEntryDraft {
    let parts = part_names
        .iter()
        .map(|part_name| item_draft(part_name))
        .collect();
    let group = PlaylistCompoundGroupDraft::new(
        PlaylistLocator::Local(LocalLocator::Native(PathBuf::from(root_name))),
        CachedPlaylistMetadata::new(root_name, PlaylistMediaKind::Audio),
        parts,
    )
    .expect("focused compound fixture всегда содержит хотя бы одну part");
    PlaylistEntryDraft::Compound(group)
}

/// Строит Single + one-part Compound + many-part Compound + Single.
fn fixture() -> CompoundFixture {
    let mut queue = PlaylistQueue::new();
    let AddPlaylistEntriesOutcome::Added(allocated) = queue
        .append_entries(vec![
            PlaylistEntryDraft::Single(item_draft("a-single.mp3")),
            compound_draft("b-one", &["b-part-1.mp3"]),
            compound_draft("c-many", &["c-part-1.mp3", "c-part-2.mp3", "c-part-3.mp3"]),
            PlaylistEntryDraft::Single(item_draft("z-single.mp3")),
        ])
        .expect("append focused S17G fixture")
    else {
        panic!("non-empty S17G fixture обязана выделить identities");
    };
    CompoundFixture {
        queue,
        entry_ids: allocated.iter_entry_ids().collect(),
        item_ids: allocated.iter_playable_item_ids().collect(),
    }
}

/// Создаёт installed identity только для проверки active projection.
fn active_identity(item_id: PlaylistItemId) -> ActiveMediaIdentity {
    let non_zero = NonZeroU64::new(1).expect("test constant is non-zero");
    ActiveMediaIdentity::installed(
        Some(item_id),
        ActiveMediaLineageId::from_non_zero(non_zero),
        MediaInstanceId::from_non_zero(non_zero),
        PlaylistBindingGeneration(1),
    )
}

/// Возвращает initial generation, общую для queue и view fixture.
const fn revision() -> PlaylistStructuralRevision {
    PlaylistStructuralRevision::INITIAL
}

/// Focused S17G fixtures не добавляют runtime error/pending state без явного теста.
fn projection_state(
    active_media: Option<ActiveMediaIdentity>,
    selection: &PlaylistSelectionSnapshot,
) -> CompoundRuntimeProjectionState<'_> {
    static EMPTY_RUNTIME_ERRORS: LazyLock<HashMap<PlaylistItemId, PlaylistItemRuntimeError>> =
        LazyLock::new(HashMap::new);
    CompoundRuntimeProjectionState {
        structural_revision: revision(),
        active_media,
        pending_target: None,
        runtime_errors: &EMPTY_RUNTIME_ERRORS,
        selection,
    }
}

/// Строит typed header action с explicit compound identity.
const fn header_action(
    compound_entry_id: PlaylistEntryId,
    structural_revision: PlaylistStructuralRevision,
) -> CompoundHeaderPlayAction {
    CompoundHeaderPlayAction {
        compound_entry_id,
        structural_revision,
    }
}

#[test]
fn collapsed_and_expanded_rows_keep_top_level_count_separate_for_one_and_many_parts() {
    let CompoundFixture {
        queue,
        entry_ids,
        item_ids,
    } = fixture();
    let mut disclosure = CompoundRuntimeViewState::default();
    let mut selection = PlaylistSelectionState::default();
    assert_eq!(
        selection.apply(
            &queue,
            revision(),
            UpdateSelection::Replace {
                entry_id: entry_ids[2],
                structural_revision: revision(),
            },
        ),
        crate::playlist_runtime::UpdateSelectionOutcome::Updated
    );

    let collapsed = disclosure.snapshot(
        &queue,
        projection_state(Some(active_identity(item_ids[3])), &selection.snapshot()),
    );
    assert_eq!(collapsed.top_level_entry_count(), 4);
    assert_eq!(collapsed.visible_row_count(), 4);
    assert!(matches!(
        collapsed.visible_rows(2..3).as_slice(),
        [CompoundRuntimeRow::CompoundHeader {
            active_part_item_id: Some(active_part_item_id),
            selected: true,
            expanded: false,
            ..
        }] if *active_part_item_id == item_ids[3]
    ));

    assert_eq!(
        disclosure.toggle(
            &queue,
            revision(),
            ToggleCompoundDisclosure {
                compound_entry_id: entry_ids[1],
                structural_revision: revision(),
            },
        ),
        ToggleCompoundDisclosureOutcome::Expanded
    );
    assert_eq!(
        disclosure.toggle(
            &queue,
            revision(),
            ToggleCompoundDisclosure {
                compound_entry_id: entry_ids[2],
                structural_revision: revision(),
            },
        ),
        ToggleCompoundDisclosureOutcome::Expanded
    );
    let expanded = disclosure.snapshot(
        &queue,
        projection_state(Some(active_identity(item_ids[3])), &selection.snapshot()),
    );
    assert_ne!(
        collapsed.layout_identity(),
        expanded.layout_identity(),
        "disclosure меняет layout identity без fake structural mutation"
    );
    assert_eq!(expanded.top_level_entry_count(), 4);
    assert_eq!(expanded.visible_row_count(), 8);
    assert!(expanded.visible_rows(0..8).iter().any(|row| matches!(
        row,
        CompoundRuntimeRow::CompoundPart {
            part_item_id,
            active: true,
            ..
        } if *part_item_id == item_ids[3]
    )));
    assert_eq!(
        (0..=expanded.visible_row_count())
            .map(|visible_slot| expanded.structural_insertion_slot(visible_slot))
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 2, 3, 3, 3, 3, 4],
        "child geometry не должна разрешать insertion внутрь compound group"
    );
}

#[test]
fn header_play_resolves_current_or_first_as_one_exact_target_without_scan() {
    let CompoundFixture {
        mut queue,
        entry_ids,
        item_ids,
    } = fixture();
    let many_entry_id = entry_ids[2];

    assert_eq!(
        resolve_header_play_target(&queue, revision(), header_action(many_entry_id, revision()),),
        CompoundHeaderPlayTarget::ExactItem(item_ids[2])
    );
    queue
        .set_traversal_current(item_ids[4])
        .expect("part belongs to committed compound");
    assert_eq!(
        resolve_header_play_target(&queue, revision(), header_action(many_entry_id, revision()),),
        CompoundHeaderPlayTarget::ExactItem(item_ids[4])
    );

    queue
        .set_traversal_current(item_ids[0])
        .expect("single belongs to committed queue");
    let first_only_target =
        resolve_header_play_target(&queue, revision(), header_action(many_entry_id, revision()));
    assert_eq!(
        first_only_target,
        CompoundHeaderPlayTarget::ExactItem(item_ids[2]),
        "resolver возвращает один strong-open target и не публикует fallback candidates"
    );
}

#[test]
fn structural_range_is_collapse_independent_and_part_play_preserves_selection() {
    let CompoundFixture {
        queue,
        entry_ids,
        item_ids,
    } = fixture();
    let mut selection = PlaylistSelectionState::default();
    let range_entries: Arc<[PlaylistEntryId]> =
        Arc::from([entry_ids[0], entry_ids[1], entry_ids[2]]);
    assert_eq!(
        selection.apply(
            &queue,
            revision(),
            UpdateSelection::ReplaceRange {
                entry_ids: Arc::clone(&range_entries),
                range_anchor: entry_ids[0],
                interaction_cursor: entry_ids[2],
                structural_revision: revision(),
            },
        ),
        crate::playlist_runtime::UpdateSelectionOutcome::Updated
    );
    let before_part_click = selection.snapshot();

    let mut disclosure = CompoundRuntimeViewState::default();
    assert_eq!(
        disclosure.toggle(
            &queue,
            revision(),
            ToggleCompoundDisclosure {
                compound_entry_id: entry_ids[2],
                structural_revision: revision(),
            },
        ),
        ToggleCompoundDisclosureOutcome::Expanded
    );
    assert_eq!(
        resolve_part_play_target(
            &queue,
            revision(),
            CompoundPartPlayAction {
                compound_entry_id: entry_ids[2],
                part_item_id: item_ids[3],
                structural_revision: revision(),
            },
        ),
        CompoundPartPlayTarget::ExactItem(item_ids[3])
    );
    let after_part_click = selection.snapshot();
    assert_eq!(
        after_part_click.selected_entry_ids(),
        before_part_click.selected_entry_ids()
    );
    assert_eq!(after_part_click.range_anchor(), Some(entry_ids[0]));
    assert_eq!(after_part_click.interaction_cursor(), Some(entry_ids[2]));

    let expanded = disclosure.snapshot(&queue, projection_state(None, &after_part_click));
    let child = expanded
        .visible_rows(0..expanded.visible_row_count())
        .into_iter()
        .find(|row| {
            matches!(
                row,
                CompoundRuntimeRow::CompoundPart { part_item_id, .. }
                    if *part_item_id == item_ids[3]
            )
        })
        .expect("expanded compound exposes exact child projection");
    assert!(child.is_subordinate_part());
    assert_eq!(child.structural_entry_id(), None);
}

#[test]
fn current_item_targets_header_when_collapsed_and_exact_part_when_expanded() {
    let CompoundFixture {
        queue,
        entry_ids,
        item_ids,
    } = fixture();
    let mut disclosure = CompoundRuntimeViewState::default();
    let selection = PlaylistSelectionState::default().snapshot();

    let collapsed = disclosure.snapshot(&queue, projection_state(None, &selection));
    assert_eq!(
        collapsed.current_item_scroll_target(item_ids[3]),
        Some(CompoundCurrentItemScrollTarget::Header(entry_ids[2]))
    );
    assert_eq!(collapsed.header_row_index(entry_ids[2]), Some(2));

    disclosure.toggle(
        &queue,
        revision(),
        ToggleCompoundDisclosure {
            compound_entry_id: entry_ids[2],
            structural_revision: revision(),
        },
    );
    let expanded = disclosure.snapshot(&queue, projection_state(None, &selection));
    assert_eq!(
        expanded.current_item_scroll_target(item_ids[3]),
        Some(CompoundCurrentItemScrollTarget::Part(item_ids[3]))
    );
}

#[test]
fn stale_group_and_part_actions_are_rejected_before_state_changes() {
    let CompoundFixture {
        queue,
        entry_ids,
        item_ids,
    } = fixture();
    let stale_revision = revision()
        .checked_next()
        .expect("initial revision has a successor");
    let mut disclosure = CompoundRuntimeViewState::default();

    assert_eq!(
        disclosure.toggle(
            &queue,
            revision(),
            ToggleCompoundDisclosure {
                compound_entry_id: entry_ids[2],
                structural_revision: stale_revision,
            },
        ),
        ToggleCompoundDisclosureOutcome::StaleStructuralRevision
    );
    assert_eq!(
        resolve_header_play_target(
            &queue,
            revision(),
            header_action(entry_ids[2], stale_revision),
        ),
        CompoundHeaderPlayTarget::StaleStructuralRevision
    );
    assert_eq!(
        resolve_header_play_target(&queue, revision(), header_action(entry_ids[0], revision()),),
        CompoundHeaderPlayTarget::NotCompoundEntry
    );
    assert_eq!(
        resolve_part_play_target(
            &queue,
            revision(),
            CompoundPartPlayAction {
                compound_entry_id: entry_ids[2],
                part_item_id: item_ids[3],
                structural_revision: stale_revision,
            },
        ),
        CompoundPartPlayTarget::StaleStructuralRevision
    );
    assert_eq!(
        resolve_part_play_target(
            &queue,
            revision(),
            CompoundPartPlayAction {
                compound_entry_id: entry_ids[2],
                part_item_id: item_ids[1],
                structural_revision: revision(),
            },
        ),
        CompoundPartPlayTarget::PartNotInGroup
    );
    let empty_selection = PlaylistSelectionState::default().snapshot();
    assert_eq!(
        disclosure
            .snapshot(&queue, projection_state(None, &empty_selection))
            .visible_row_count(),
        4,
        "stale toggle не должен раскрывать group"
    );
}
