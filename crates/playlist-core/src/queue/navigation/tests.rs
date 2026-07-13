//! Focused transition-matrix tests canonical navigation/repeat.

use crate::{
    AutomaticEndedIntent, AutomaticNavigationOutcome, AutomaticStopReason, CachedPlaylistMetadata,
    LocalLocator, ManualNavigationDirection, ManualNavigationIntent, ManualNavigationNoItem,
    ManualNavigationOutcome, ManualNavigationPreviewState, PlaylistItemDraft, PlaylistMediaKind,
    PlaylistQueue, RepeatMode, TraversalCurrentMutationOutcome,
};

/// Создаёт bounded local draft с читаемой canonical меткой.
fn local_draft(label: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(format!("/test/{label}.mp4").into()),
        None,
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
    )
}

/// Создаёт очередь указанного размера и возвращает её canonical IDs.
fn queue_with_items(item_count: usize) -> (PlaylistQueue, Vec<crate::PlaylistItemId>) {
    let mut queue = PlaylistQueue::new();
    let drafts = (0..item_count)
        .map(|index| local_draft(&format!("item-{index}")))
        .collect();
    let outcome = queue.append_batch(drafts).expect("append test queue");
    let ids = match outcome {
        crate::AddItemsOutcome::Added(ids) => ids.into_vec(),
        crate::AddItemsOutcome::NoItemsProvided => Vec::new(),
    };
    (queue, ids)
}

/// Устанавливает committed current для конкретной matrix строки.
fn set_current(queue: &mut PlaylistQueue, item_id: crate::PlaylistItemId) {
    assert!(matches!(
        queue
            .set_traversal_current(item_id)
            .expect("set matrix current"),
        TraversalCurrentMutationOutcome::Set(_)
            | TraversalCurrentMutationOutcome::AlreadyCurrent(_)
    ));
}

/// Извлекает preview из typed manual OpenItem outcome.
fn expect_manual_open(
    outcome: ManualNavigationOutcome,
    expected_item_id: crate::PlaylistItemId,
) -> crate::ManualNavigationPreview {
    match outcome {
        ManualNavigationOutcome::OpenItem { item_id, preview } => {
            assert_eq!(item_id, expected_item_id);
            preview
        }
        ManualNavigationOutcome::NoItem(reason) => {
            panic!("expected OpenItem, received NoItem({reason:?})")
        }
    }
}

#[test]
fn automatic_transition_matrix_covers_empty_one_and_canonical_positions() {
    let repeat_modes = [
        RepeatMode::StopAtEnd,
        RepeatMode::RepeatQueue,
        RepeatMode::RepeatOne,
    ];
    let empty_queue = PlaylistQueue::new();
    for repeat_mode in repeat_modes {
        assert_eq!(
            empty_queue.automatic_navigation(AutomaticEndedIntent::new(repeat_mode)),
            AutomaticNavigationOutcome::Stop(AutomaticStopReason::EmptyQueue)
        );
    }

    let (mut one_queue, one_ids) = queue_with_items(1);
    set_current(&mut one_queue, one_ids[0]);
    assert_eq!(
        one_queue.automatic_navigation(AutomaticEndedIntent::new(RepeatMode::StopAtEnd)),
        AutomaticNavigationOutcome::Stop(AutomaticStopReason::EndOfQueue {
            current_item_id: one_ids[0]
        })
    );
    assert_eq!(
        one_queue.automatic_navigation(AutomaticEndedIntent::new(RepeatMode::RepeatQueue)),
        AutomaticNavigationOutcome::OpenItem {
            item_id: one_ids[0]
        }
    );
    assert_eq!(
        one_queue.automatic_navigation(AutomaticEndedIntent::new(RepeatMode::RepeatOne)),
        AutomaticNavigationOutcome::ReplayCurrent {
            item_id: one_ids[0]
        }
    );

    let (mut queue, ids) = queue_with_items(3);
    for current_index in 0..ids.len() {
        set_current(&mut queue, ids[current_index]);
        for repeat_mode in repeat_modes {
            let actual = queue.automatic_navigation(AutomaticEndedIntent::new(repeat_mode));
            let expected = if repeat_mode == RepeatMode::RepeatOne {
                AutomaticNavigationOutcome::ReplayCurrent {
                    item_id: ids[current_index],
                }
            } else if current_index + 1 < ids.len() {
                AutomaticNavigationOutcome::OpenItem {
                    item_id: ids[current_index + 1],
                }
            } else if repeat_mode == RepeatMode::RepeatQueue {
                AutomaticNavigationOutcome::OpenItem { item_id: ids[0] }
            } else {
                AutomaticNavigationOutcome::Stop(AutomaticStopReason::EndOfQueue {
                    current_item_id: ids[current_index],
                })
            };
            assert_eq!(
                actual, expected,
                "index={current_index}, repeat={repeat_mode:?}"
            );
        }
    }
}

#[test]
fn automatic_persisted_current_none_stops_in_every_repeat_mode() {
    let (queue, _) = queue_with_items(3);
    for repeat_mode in [
        RepeatMode::StopAtEnd,
        RepeatMode::RepeatQueue,
        RepeatMode::RepeatOne,
    ] {
        assert_eq!(
            queue.automatic_navigation(AutomaticEndedIntent::new(repeat_mode)),
            AutomaticNavigationOutcome::Stop(AutomaticStopReason::CurrentItemAbsent)
        );
    }
}

#[test]
fn manual_matrix_covers_empty_and_persisted_idle_semantics() {
    let empty_queue = PlaylistQueue::new();
    for repeat_mode in [
        RepeatMode::StopAtEnd,
        RepeatMode::RepeatQueue,
        RepeatMode::RepeatOne,
    ] {
        for intent in [
            ManualNavigationIntent::next(repeat_mode),
            ManualNavigationIntent::previous(repeat_mode),
        ] {
            assert!(matches!(
                empty_queue.begin_manual_navigation(intent),
                ManualNavigationOutcome::NoItem(ManualNavigationNoItem::EmptyQueue)
            ));
        }
    }

    let (queue, ids) = queue_with_items(3);
    for repeat_mode in [
        RepeatMode::StopAtEnd,
        RepeatMode::RepeatQueue,
        RepeatMode::RepeatOne,
    ] {
        let preview = expect_manual_open(
            queue.begin_manual_navigation(ManualNavigationIntent::next(repeat_mode)),
            ids[0],
        );
        assert_eq!(
            preview.origin(),
            crate::ManualNavigationOrigin::PersistedIdle
        );
        assert!(matches!(
            queue.begin_manual_navigation(ManualNavigationIntent::previous(repeat_mode)),
            ManualNavigationOutcome::NoItem(ManualNavigationNoItem::PreviousFromPersistedIdle)
        ));
    }
}

#[test]
fn manual_matrix_ignores_repeat_one_inside_queue_and_wraps_only_repeat_queue() {
    let (mut queue, ids) = queue_with_items(3);
    for current_index in 0..ids.len() {
        set_current(&mut queue, ids[current_index]);
        for repeat_mode in [
            RepeatMode::StopAtEnd,
            RepeatMode::RepeatQueue,
            RepeatMode::RepeatOne,
        ] {
            for direction in [
                ManualNavigationDirection::Next,
                ManualNavigationDirection::Previous,
            ] {
                let intent = match direction {
                    ManualNavigationDirection::Next => ManualNavigationIntent::next(repeat_mode),
                    ManualNavigationDirection::Previous => {
                        ManualNavigationIntent::previous(repeat_mode)
                    }
                };
                let actual = queue.begin_manual_navigation(intent);
                let expected_index = match direction {
                    ManualNavigationDirection::Next if current_index + 1 < ids.len() => {
                        Some(current_index + 1)
                    }
                    ManualNavigationDirection::Previous if current_index > 0 => {
                        Some(current_index - 1)
                    }
                    ManualNavigationDirection::Next if repeat_mode == RepeatMode::RepeatQueue => {
                        Some(0)
                    }
                    ManualNavigationDirection::Previous
                        if repeat_mode == RepeatMode::RepeatQueue =>
                    {
                        Some(ids.len() - 1)
                    }
                    _ => None,
                };
                match expected_index {
                    Some(target_index) => {
                        let _preview = expect_manual_open(actual, ids[target_index]);
                    }
                    None => assert!(matches!(
                        actual,
                        ManualNavigationOutcome::NoItem(
                            ManualNavigationNoItem::QueueBoundary {
                                current_item_id,
                                direction: actual_direction,
                            }
                        ) if current_item_id == ids[current_index]
                            && actual_direction == direction
                    )),
                }
            }
        }
    }
}

#[test]
fn one_item_manual_navigation_wraps_only_for_repeat_queue() {
    let (mut queue, ids) = queue_with_items(1);
    set_current(&mut queue, ids[0]);
    for direction in [
        ManualNavigationDirection::Next,
        ManualNavigationDirection::Previous,
    ] {
        for repeat_mode in [RepeatMode::StopAtEnd, RepeatMode::RepeatOne] {
            let intent = match direction {
                ManualNavigationDirection::Next => ManualNavigationIntent::next(repeat_mode),
                ManualNavigationDirection::Previous => {
                    ManualNavigationIntent::previous(repeat_mode)
                }
            };
            assert!(matches!(
                queue.begin_manual_navigation(intent),
                ManualNavigationOutcome::NoItem(ManualNavigationNoItem::QueueBoundary { .. })
            ));
        }
        let repeat_queue_intent = match direction {
            ManualNavigationDirection::Next => {
                ManualNavigationIntent::next(RepeatMode::RepeatQueue)
            }
            ManualNavigationDirection::Previous => {
                ManualNavigationIntent::previous(RepeatMode::RepeatQueue)
            }
        };
        let _preview =
            expect_manual_open(queue.begin_manual_navigation(repeat_queue_intent), ids[0]);
    }
}

#[test]
fn fast_preview_advances_from_latest_target_and_backtracks_to_origin() {
    let (mut queue, ids) = queue_with_items(3);
    set_current(&mut queue, ids[0]);
    let revision_before = queue.revision_snapshot();
    let preview_b = expect_manual_open(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
        ids[1],
    );
    let preview_c = expect_manual_open(
        queue
            .continue_manual_navigation(
                preview_b,
                ManualNavigationIntent::next(RepeatMode::StopAtEnd),
            )
            .expect("advance B to C"),
        ids[2],
    );
    let preview_b = expect_manual_open(
        queue
            .continue_manual_navigation(
                preview_c,
                ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
            )
            .expect("backtrack C to B"),
        ids[1],
    );
    assert!(matches!(
        queue
            .continue_manual_navigation(
                preview_b,
                ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
            )
            .expect("backtrack B to origin A"),
        ManualNavigationOutcome::NoItem(
            ManualNavigationNoItem::ReturnedToCommittedOrigin { item_id }
        ) if item_id == ids[0]
    ));
    assert_eq!(queue.traversal_current().unwrap().item_id(), ids[0]);
    assert_eq!(queue.revision_snapshot(), revision_before);
}

#[test]
fn discard_and_failure_preserve_committed_queue_without_domain_mutation() {
    let (mut queue, ids) = queue_with_items(3);
    set_current(&mut queue, ids[0]);
    let revision_before = queue.revision_snapshot();
    let preview_b = expect_manual_open(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
        ids[1],
    );
    let discarded = queue.discard_manual_navigation(preview_b);
    assert_eq!(discarded.latest_target_item_id(), ids[1]);
    assert_eq!(discarded.state(), ManualNavigationPreviewState::Ready);
    assert_eq!(queue.revision_snapshot(), revision_before);

    let preview_b = expect_manual_open(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
        ids[1],
    );
    let preview_c = expect_manual_open(
        queue
            .continue_manual_navigation(
                preview_b,
                ManualNavigationIntent::next(RepeatMode::StopAtEnd),
            )
            .expect("advance B to C before failure"),
        ids[2],
    );
    let token = queue
        .prepare_manual_navigation(preview_c)
        .expect("prepare C navigation");
    assert_eq!(queue.traversal_current().unwrap().item_id(), ids[0]);
    let failed_preview = queue.fail_manual_navigation(token);
    assert_eq!(
        failed_preview.state(),
        ManualNavigationPreviewState::AwaitingUserAfterFailure(
            crate::FailedManualNavigationTarget { item_id: ids[2] }
        )
    );
    assert_eq!(queue.traversal_current().unwrap().item_id(), ids[0]);
    assert_eq!(queue.revision_snapshot(), revision_before);

    let preview_b = expect_manual_open(
        queue
            .continue_manual_navigation(
                failed_preview,
                ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
            )
            .expect("backtrack after failed C"),
        ids[1],
    );
    assert_eq!(preview_b.state(), ManualNavigationPreviewState::Ready);
    let _discarded = queue.discard_manual_navigation(preview_b);
    assert_eq!(queue.revision_snapshot(), revision_before);
}

#[test]
fn current_changes_only_after_one_explicit_success_commit() {
    let (mut queue, ids) = queue_with_items(3);
    set_current(&mut queue, ids[0]);
    let traversal_revision_before = queue.revision_snapshot().traversal();
    let preview_b = expect_manual_open(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
        ids[1],
    );
    let preview_c = expect_manual_open(
        queue
            .continue_manual_navigation(
                preview_b,
                ManualNavigationIntent::next(RepeatMode::StopAtEnd),
            )
            .expect("advance to C"),
        ids[2],
    );
    assert_eq!(queue.traversal_current().unwrap().item_id(), ids[0]);
    let token = queue
        .prepare_manual_navigation(preview_c)
        .expect("prepare latest C target");
    assert_eq!(token.target_item_id(), ids[2]);
    assert_eq!(queue.traversal_current().unwrap().item_id(), ids[0]);
    let commit = queue.commit_manual_navigation(token);
    assert_eq!(commit.traversal_current().item_id(), ids[2]);
    assert_eq!(queue.traversal_current().unwrap().item_id(), ids[2]);
    assert_ne!(
        queue.revision_snapshot().traversal(),
        traversal_revision_before
    );
}

#[test]
fn structural_change_invalidates_preview() {
    let (mut queue, ids) = queue_with_items(3);
    set_current(&mut queue, ids[0]);
    let preview_b = expect_manual_open(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
        ids[1],
    );
    assert!(matches!(
        queue.move_item(ids[2], crate::MoveItemIntent::ToFront),
        crate::MoveItemOutcome::Moved { item_id } if item_id == ids[2]
    ));
    assert!(matches!(
        queue.continue_manual_navigation(
            preview_b,
            ManualNavigationIntent::next(RepeatMode::StopAtEnd)
        ),
        Err(crate::ManualNavigationPreviewError::QueueChanged { .. })
    ));
}

#[test]
fn prepare_failure_returns_preview_and_preserves_committed_current() {
    let (mut queue, ids) = queue_with_items(3);
    set_current(&mut queue, ids[0]);
    let preview_b = expect_manual_open(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
        ids[1],
    );
    let blocking_token = queue
        .prepare_reserved_mutation(
            queue.revision_snapshot(),
            crate::ReservedQueueMutation::select_committed(ids[0]),
        )
        .expect("install blocking reservation");
    let failure = queue
        .prepare_manual_navigation(preview_b)
        .expect_err("second reservation must be rejected");
    assert_eq!(
        failure.reason(),
        crate::PrepareReservedMutationError::InstallCommitLinearizing
    );
    let returned_preview = failure.into_preview();
    assert_eq!(returned_preview.latest_target_item_id(), ids[1]);
    assert_eq!(queue.traversal_current().unwrap().item_id(), ids[0]);
    queue.abort_reserved(blocking_token);
    assert_eq!(queue.traversal_current().unwrap().item_id(), ids[0]);
}
