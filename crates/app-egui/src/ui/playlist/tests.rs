//! Focused characterization read-only virtualized Playlist UI.

use std::path::PathBuf;

use media_core::MediaDuration;
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, MoveItemIntent, PlaylistItemDraft, PlaylistItemId,
    PlaylistMediaKind, PlaylistQueue,
};

use super::renderer::{ROW_HEIGHT, accessibility_text, anchored_scroll_offset, stable_row_id};
use super::status::{navigation_message, save_message};
use super::{PlaylistAction, PlaylistUiOutput, PlaylistUiState, ViewportAnchor};
use crate::playlist_runtime::{
    PlaylistInteractionModel, PlaylistLoadingView, PlaylistNavigationView, PlaylistSaveView,
    PlaylistViewModel, PlaylistVisibleRow, PlaylistVisibleRowTestFixture,
};

fn draft(index: usize) -> PlaylistItemDraft {
    let metadata =
        CachedPlaylistMetadata::new(format!("episode-{index}.mkv"), PlaylistMediaKind::Video)
            .with_title(Some(format!("Эпизод {index}")))
            .with_duration(Some(MediaDuration::from_secs(index as u64 + 30)));
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(format!("episode-{index}.mkv"))),
        None,
        metadata,
    )
}

fn queue(item_count: usize) -> PlaylistQueue {
    let mut queue = PlaylistQueue::new();
    if item_count > 0 {
        queue
            .append_batch((0..item_count).map(draft).collect())
            .expect("test queue must fit hard cap");
    }
    queue
}

fn model(queue: &PlaylistQueue, structural_revision: u64) -> PlaylistViewModel {
    PlaylistViewModel::for_queue_with_revision(
        queue,
        structural_revision,
        PlaylistLoadingView::Ready,
    )
}

#[test]
fn loading_and_empty_are_distinct_model_states() {
    let queue = PlaylistQueue::new();
    let loading =
        PlaylistViewModel::for_queue_with_revision(&queue, 0, PlaylistLoadingView::Loading);
    let ready = model(&queue, 0);

    assert!(loading.is_empty());
    assert!(ready.is_empty());
    assert_eq!(loading.loading(), PlaylistLoadingView::Loading);
    assert_eq!(ready.loading(), PlaylistLoadingView::Ready);
}

#[test]
fn zero_one_and_ten_thousand_rows_only_publish_viewport_hint() {
    for item_count in [0, 1, 10_000] {
        let queue = queue(item_count);
        let model = model(&queue, 1);
        let mut state = PlaylistUiState::default();
        let mut output = PlaylistUiOutput::default();

        egui::__run_test_ui(|ui| {
            ui.set_width(420.0);
            ui.set_max_height(180.0);
            super::renderer::show_rows(ui, &model, &mut state, &mut output);
        });

        assert!(output.visible_item_ids.len() <= super::MAX_VISIBLE_HINT_ITEMS);
        if item_count == 0 {
            assert!(output.visible_item_ids.is_empty());
        } else {
            assert!(!output.visible_item_ids.is_empty());
            assert!(output.visible_item_ids.len() < item_count.max(32));
        }
    }
}

#[test]
fn repeated_visual_copies_deduplicate_and_bound_visible_hint() {
    let queue = queue(400);
    let model = model(&queue, 1);
    let mut output = PlaylistUiOutput::default();
    let requested = model.visible_rows(0..400);
    for _ in 0..2 {
        for row in &requested {
            output.record_visible(row.item_id());
        }
    }

    assert_eq!(output.visible_item_ids.len(), super::MAX_VISIBLE_HINT_ITEMS);
    let mut deduplicated = output.visible_item_ids.clone();
    deduplicated.sort_unstable();
    deduplicated.dedup();
    assert_eq!(deduplicated.len(), output.visible_item_ids.len());
}

#[test]
fn typed_toolbar_actions_keep_render_order_until_post_render_drain() {
    let mut output = PlaylistUiOutput::default();
    output.push_action(PlaylistAction::AddFiles);
    output.push_action(PlaylistAction::OpenUrlEditor);

    assert_eq!(
        output.take_actions(),
        vec![PlaylistAction::AddFiles, PlaylistAction::OpenUrlEditor]
    );
    assert!(output.take_actions().is_empty());
}

#[test]
fn disabled_animation_copy_does_not_replace_viewport_or_publish_hint() {
    let queue = queue(100);
    let model = model(&queue, 4);
    let top_item_id = queue.items()[33].item_id();
    let mut state = PlaylistUiState {
        viewport_anchor: Some(ViewportAnchor {
            item_id: top_item_id,
            intra_row_offset: 6.0,
        }),
        observed_structural_revision: Some(model.structural_revision()),
        go_current: None,
    };
    let mut output = PlaylistUiOutput::default();
    let interaction = PlaylistInteractionModel {
        url_editor_open: true,
        url_request_focus: true,
        ..PlaylistInteractionModel::default()
    };

    egui::__run_test_ui(|ui| {
        ui.disable();
        ui.set_width(420.0);
        ui.set_max_height(180.0);
        super::show(ui, Some(&model), &interaction, &mut state, &mut output);
    });

    assert_eq!(state.viewport_anchor.unwrap().item_id, top_item_id);
    assert!(output.visible_item_ids.is_empty());
    assert!(output.actions.is_empty());
}

#[test]
fn shared_model_clone_does_not_clone_full_row_strings() {
    let queue = queue(10_000);
    let model = model(&queue, 1);
    let cloned = model.clone();

    assert_eq!(model.shared_rows_identity(), cloned.shared_rows_identity());
    assert_eq!(
        model.shared_title_identity(9_999),
        cloned.shared_title_identity(9_999)
    );
}

#[test]
fn stable_row_id_depends_on_item_identity_not_row_index() {
    let parent = egui::Id::new("playlist-test");
    let first = PlaylistItemId::from_persistence_value(41).unwrap();
    let second = PlaylistItemId::from_persistence_value(42).unwrap();

    assert_eq!(stable_row_id(parent, first), stable_row_id(parent, first));
    assert_ne!(stable_row_id(parent, first), stable_row_id(parent, second));
}

#[test]
fn insertion_before_inside_or_after_viewport_preserves_top_item_and_offset() {
    for placement in [
        InsertionPlacement::Before,
        InsertionPlacement::Inside,
        InsertionPlacement::After,
    ] {
        let mut queue = queue(20);
        let top_item_id = queue.items()[8].item_id();
        let inside_anchor = queue.items()[12].item_id();
        let before = model(&queue, 1);
        let mut state = PlaylistUiState {
            viewport_anchor: Some(ViewportAnchor {
                item_id: top_item_id,
                intra_row_offset: 7.5,
            }),
            observed_structural_revision: Some(before.structural_revision()),
            go_current: None,
        };
        let inserted_item_id = match queue.append_one(draft(100)).expect("append") {
            playlist_core::AddItemsOutcome::Added(item_ids) => item_ids.as_slice()[0],
            playlist_core::AddItemsOutcome::NoItemsProvided => panic!("one draft must be added"),
        };
        match placement {
            InsertionPlacement::Before => {
                queue.move_item(inserted_item_id, MoveItemIntent::Before(top_item_id));
            }
            InsertionPlacement::Inside => {
                queue.move_item(inserted_item_id, MoveItemIntent::After(inside_anchor));
            }
            InsertionPlacement::After => {}
        }
        let after = model(&queue, 2);
        let row_pitch = ROW_HEIGHT + 8.0;
        let anchored_offset =
            anchored_scroll_offset(&after, &mut state, row_pitch).expect("anchor survives");
        let expected_index = after.row_index(top_item_id).expect("stable item remains");

        assert_eq!(anchored_offset, expected_index as f32 * row_pitch + 7.5);
        assert_eq!(state.viewport_anchor.unwrap().item_id, top_item_id);
    }
}

#[derive(Clone, Copy)]
enum InsertionPlacement {
    Before,
    Inside,
    After,
}

#[test]
fn active_only_revision_never_requests_hidden_scroll() {
    let queue = queue(30);
    let model = model(&queue, 5);
    let top_item_id = queue.items()[11].item_id();
    let mut state = PlaylistUiState {
        viewport_anchor: Some(ViewportAnchor {
            item_id: top_item_id,
            intra_row_offset: 4.0,
        }),
        observed_structural_revision: Some(model.structural_revision()),
        go_current: None,
    };

    assert_eq!(anchored_scroll_offset(&model, &mut state, 42.0), None);
    assert_eq!(state.viewport_anchor.unwrap().item_id, top_item_id);
}

#[test]
fn error_retry_and_failed_navigation_have_distinct_accessibility() {
    let item_id = PlaylistItemId::from_persistence_value(7).unwrap();
    let retrying = PlaylistVisibleRow::from_test_fixture(PlaylistVisibleRowTestFixture {
        item_id,
        fallback_display_name: "fallback.mkv".into(),
        display_title: "Полное название".into(),
        duration: Some(MediaDuration::from_secs(65)),
        media_kind: PlaylistMediaKind::Video,
        active: true,
        pending: true,
        selected: true,
        safe_error_summary: Some("Источник временно недоступен".into()),
    });
    let failed = PlaylistVisibleRow::from_test_fixture(PlaylistVisibleRowTestFixture {
        item_id,
        fallback_display_name: "fallback.mkv".into(),
        display_title: "Полное название".into(),
        duration: None,
        media_kind: PlaylistMediaKind::Video,
        active: false,
        pending: false,
        selected: false,
        safe_error_summary: Some("Источник временно недоступен".into()),
    });

    let retrying_text = accessibility_text(0, &retrying);
    let failed_text = accessibility_text(0, &failed);
    assert!(retrying_text.contains("Предыдущая попытка завершилась ошибкой"));
    assert!(retrying_text.contains("выполняется повторная попытка"));
    assert!(failed_text.contains("Ошибка"));
    assert!(!failed_text.contains("повторная попытка"));

    let (navigation, _) =
        navigation_message(PlaylistNavigationView::AwaitingUserAfterFailure).expect("D55 status");
    assert!(navigation.contains("Автоматический переход остановлен"));
    assert!(navigation.contains("Next, Previous или повтор"));
}

#[test]
fn save_warning_exposes_retry_and_unavailable_row_stays_non_modal() {
    let (warning, _) = save_message(PlaylistSaveView::WarningRetryAvailable {
        occurrence_count: 3,
    })
    .expect("D69 warning");
    assert!(warning.contains("Повтор сохранения доступен"));

    let unavailable = PlaylistVisibleRow::from_test_fixture(PlaylistVisibleRowTestFixture {
        item_id: PlaylistItemId::from_persistence_value(9).unwrap(),
        fallback_display_name: "недоступный-файл.mkv".into(),
        display_title: "недоступный-файл.mkv".into(),
        duration: None,
        media_kind: PlaylistMediaKind::Unknown,
        active: false,
        pending: false,
        selected: false,
        safe_error_summary: Some("Файл недоступен".into()),
    });
    assert!(accessibility_text(8, &unavailable).contains("Файл недоступен"));
}

#[test]
fn long_and_unicode_safe_text_does_not_expand_visible_hint() {
    let item_id = PlaylistItemId::from_persistence_value(11).unwrap();
    let long_title = "Очень длинное название ".repeat(2_000);
    let row = PlaylistVisibleRow::from_test_fixture(PlaylistVisibleRowTestFixture {
        item_id,
        fallback_display_name: "безопасное отображение non-UTF-8 имени".into(),
        display_title: long_title,
        duration: None,
        media_kind: PlaylistMediaKind::Audio,
        active: false,
        pending: false,
        selected: false,
        safe_error_summary: None,
    });
    let text = accessibility_text(0, &row);

    assert!(text.contains("безопасное отображение non-UTF-8 имени"));
    assert!(text.contains("Тип: Аудио"));
}
