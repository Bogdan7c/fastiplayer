//! Focused characterization read-only virtualized Playlist UI.

use std::path::PathBuf;

use egui::{Event, Key, Modifiers, PointerButton, RawInput, Rect, pos2, vec2};
use media_core::MediaDuration;
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, MoveItemIntent, PlaylistItemDraft, PlaylistItemId,
    PlaylistMediaKind, PlaylistQueue,
};
use ui_artwork_egui::MediaKindGlyph;

use super::renderer::{
    INDEX_WIDTH, MEDIA_KIND_WIDTH, ROW_HEIGHT, TOOLTIP_MAX_WIDTH, accessibility_text,
    anchored_scroll_offset, media_kind_glyph, show_rows, stable_row_id, tooltip_width,
};
use super::status::{navigation_message, save_message};
use super::{PlaylistAction, PlaylistUiOutput, PlaylistUiState, ViewportAnchor};
use crate::playlist_runtime::{
    PlaylistInteractionModel, PlaylistLoadingView, PlaylistNavigationView, PlaylistSaveView,
    PlaylistViewModel, PlaylistVisibleRow, PlaylistVisibleRowTestFixture,
};
use crate::ui::skin::{MinimalSkin, PlayerSkin, PlaylistRowStyle};

/// Focused UI tests используют те же explicit row tokens, что production MinimalSkin.
fn row_style() -> PlaylistRowStyle {
    MinimalSkin.playlist_row_style()
}

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

/// Формирует deterministic viewport для настоящего headless egui interaction pass.
fn playlist_raw_input(events: Vec<Event>, time: f64) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 120.0))),
        time: Some(time),
        events,
        ..RawInput::default()
    }
}

/// Строит pointer press/release с явной кнопкой для primary и context-menu сценариев.
fn pointer_button(position: egui::Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton {
        pos: position,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

/// Создаёт один non-repeat keyboard press с logical platform modifiers.
fn key_press(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// Рендерит production playlist rows и возвращает только typed post-render actions кадра.
fn render_playlist_input(
    context: &egui::Context,
    model: &PlaylistViewModel,
    state: &mut PlaylistUiState,
    input: RawInput,
) -> Vec<PlaylistAction> {
    let mut output = PlaylistUiOutput::default();
    let _ = context.run_ui(input, |ui| {
        // Фиксированный размер делает координаты колонок стабильными между input frames.
        ui.set_width(420.0);
        ui.set_height(120.0);
        show_rows(ui, model, row_style(), state, &mut output);
    });
    output.take_actions()
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
fn full_row_primary_click_hits_index_icon_title_duration_badges_and_trailing_edge() {
    let queue = queue(1);
    let model = model(&queue, 1);
    let item_id = model.item_id_at(0).expect("fixture row");

    // Точки покрывают все визуальные колонки и край строки, не завися от дочерних Label.
    for (case_index, x) in [2.0, 44.0, 70.0, 210.0, 350.0, 395.0]
        .into_iter()
        .enumerate()
    {
        let context = egui::Context::default();
        let mut state = PlaylistUiState::default();
        let position = pos2(x, ROW_HEIGHT * 0.5);
        let base_time = case_index as f64;

        let hover_actions = render_playlist_input(
            &context,
            &model,
            &mut state,
            playlist_raw_input(vec![Event::PointerMoved(position)], base_time),
        );
        assert!(hover_actions.is_empty(), "hover at x={x}");
        let press_actions = render_playlist_input(
            &context,
            &model,
            &mut state,
            playlist_raw_input(
                vec![pointer_button(position, PointerButton::Primary, true)],
                base_time + 0.01,
            ),
        );
        assert!(press_actions.is_empty(), "press at x={x}");
        let release_actions = render_playlist_input(
            &context,
            &model,
            &mut state,
            playlist_raw_input(
                vec![pointer_button(position, PointerButton::Primary, false)],
                base_time + 0.02,
            ),
        );
        assert!(
            matches!(
                release_actions.as_slice(),
                [PlaylistAction::UpdateSelection(
                    crate::playlist_runtime::UpdateSelection::Replace {
                        item_id: clicked_item_id,
                        ..
                    }
                )] if *clicked_item_id == item_id
            ),
            "release at x={x} produced {release_actions:?}"
        );
    }
}

#[test]
fn full_row_double_secondary_and_drag_use_the_same_stable_hit_area() {
    let queue = queue(3);
    let model = model(&queue, 1);
    let first_item_id = model.item_id_at(0).expect("first row");

    // Двойной primary click на правом краю запускает конкретную stable-ID строку.
    let double_context = egui::Context::default();
    let mut double_state = PlaylistUiState::default();
    let edge = pos2(395.0, ROW_HEIGHT * 0.5);
    render_playlist_input(
        &double_context,
        &model,
        &mut double_state,
        playlist_raw_input(vec![Event::PointerMoved(edge)], 1.0),
    );
    for (pressed, time) in [(true, 1.01), (false, 1.02), (true, 1.10)] {
        render_playlist_input(
            &double_context,
            &model,
            &mut double_state,
            playlist_raw_input(
                vec![pointer_button(edge, PointerButton::Primary, pressed)],
                time,
            ),
        );
    }
    let double_actions = render_playlist_input(
        &double_context,
        &model,
        &mut double_state,
        playlist_raw_input(
            vec![pointer_button(edge, PointerButton::Primary, false)],
            1.11,
        ),
    );
    assert!(
        double_actions.contains(&PlaylistAction::Play(first_item_id)),
        "double click produced {double_actions:?}"
    );

    // Secondary click на том же краю сначала выбирает невыбранную строку.
    let secondary_context = egui::Context::default();
    let mut secondary_state = PlaylistUiState::default();
    render_playlist_input(
        &secondary_context,
        &model,
        &mut secondary_state,
        playlist_raw_input(vec![Event::PointerMoved(edge)], 2.0),
    );
    render_playlist_input(
        &secondary_context,
        &model,
        &mut secondary_state,
        playlist_raw_input(
            vec![pointer_button(edge, PointerButton::Secondary, true)],
            2.01,
        ),
    );
    let secondary_actions = render_playlist_input(
        &secondary_context,
        &model,
        &mut secondary_state,
        playlist_raw_input(
            vec![pointer_button(edge, PointerButton::Secondary, false)],
            2.02,
        ),
    );
    assert!(matches!(
        secondary_actions.as_slice(),
        [PlaylistAction::UpdateSelection(
            crate::playlist_runtime::UpdateSelection::Replace {
                item_id,
                ..
            }
        )] if *item_id == first_item_id
    ));

    // Drag с title-column захватывает singleton и публикует group MoveItems на release.
    let drag_context = egui::Context::default();
    let mut drag_state = PlaylistUiState::default();
    let drag_start = pos2(210.0, ROW_HEIGHT * 0.5);
    let drag_end = pos2(210.0, ROW_HEIGHT * 2.5);
    render_playlist_input(
        &drag_context,
        &model,
        &mut drag_state,
        playlist_raw_input(vec![Event::PointerMoved(drag_start)], 3.0),
    );
    render_playlist_input(
        &drag_context,
        &model,
        &mut drag_state,
        playlist_raw_input(
            vec![pointer_button(drag_start, PointerButton::Primary, true)],
            3.01,
        ),
    );
    let drag_actions = render_playlist_input(
        &drag_context,
        &model,
        &mut drag_state,
        playlist_raw_input(vec![Event::PointerMoved(drag_end)], 3.02),
    );
    assert!(drag_actions.iter().any(|action| matches!(
        action,
        PlaylistAction::UpdateSelection(crate::playlist_runtime::UpdateSelection::Replace {
            item_id,
            ..
        }) if *item_id == first_item_id
    )));
    let drop_actions = render_playlist_input(
        &drag_context,
        &model,
        &mut drag_state,
        playlist_raw_input(
            vec![pointer_button(drag_end, PointerButton::Primary, false)],
            3.03,
        ),
    );
    assert!(
        drop_actions
            .iter()
            .any(|action| matches!(action, PlaylistAction::MoveItems(_))),
        "drop produced {drop_actions:?}"
    );
}

#[test]
fn focused_row_keyboard_navigation_select_all_and_empty_area_click_emit_typed_intents() {
    let queue = queue(3);
    let model = model(&queue, 1);
    let context = egui::Context::default();
    let mut state = PlaylistUiState::default();
    let first_row = pos2(180.0, ROW_HEIGHT * 0.5);

    // Реальный click даёт full-row widget keyboard focus для следующего input frame.
    render_playlist_input(
        &context,
        &model,
        &mut state,
        playlist_raw_input(vec![Event::PointerMoved(first_row)], 1.0),
    );
    render_playlist_input(
        &context,
        &model,
        &mut state,
        playlist_raw_input(
            vec![pointer_button(first_row, PointerButton::Primary, true)],
            1.01,
        ),
    );
    render_playlist_input(
        &context,
        &model,
        &mut state,
        playlist_raw_input(
            vec![pointer_button(first_row, PointerButton::Primary, false)],
            1.02,
        ),
    );

    let navigation_actions = render_playlist_input(
        &context,
        &model,
        &mut state,
        playlist_raw_input(vec![key_press(Key::ArrowDown, Modifiers::NONE)], 1.03),
    );
    let second_item_id = model.item_id_at(1).expect("second row");
    assert!(matches!(
        navigation_actions.as_slice(),
        [PlaylistAction::UpdateSelection(
            crate::playlist_runtime::UpdateSelection::Replace {
                item_id,
                ..
            }
        )] if *item_id == second_item_id
    ));

    let select_all_actions = render_playlist_input(
        &context,
        &model,
        &mut state,
        playlist_raw_input(vec![key_press(Key::A, Modifiers::COMMAND)], 1.04),
    );
    assert!(matches!(
        select_all_actions.as_slice(),
        [PlaylistAction::UpdateSelection(
            crate::playlist_runtime::UpdateSelection::SelectAll {
                item_ids,
                ..
            }
        )] if item_ids.len() == 3
    ));

    // Пространство после последней строки принадлежит list background, а не последней row.
    let empty_area = pos2(180.0, 110.0);
    render_playlist_input(
        &context,
        &model,
        &mut state,
        playlist_raw_input(vec![Event::PointerMoved(empty_area)], 2.0),
    );
    render_playlist_input(
        &context,
        &model,
        &mut state,
        playlist_raw_input(
            vec![pointer_button(empty_area, PointerButton::Primary, true)],
            2.01,
        ),
    );
    let clear_actions = render_playlist_input(
        &context,
        &model,
        &mut state,
        playlist_raw_input(
            vec![pointer_button(empty_area, PointerButton::Primary, false)],
            2.02,
        ),
    );
    assert_eq!(
        clear_actions,
        vec![PlaylistAction::UpdateSelection(
            crate::playlist_runtime::UpdateSelection::Clear {
                cursor: crate::playlist_runtime::ClearSelectionCursor::Clear,
            }
        )]
    );
}

#[test]
fn row_content_labels_explicitly_disable_text_selection() {
    let renderer_source = include_str!("renderer.rs");
    let row_content_start = renderer_source
        .find("fn render_row_content")
        .expect("row content symbol");
    let tooltip_start = renderer_source
        .find("fn show_safe_tooltip")
        .expect("tooltip symbol");
    let row_content_source = &renderer_source[row_content_start..tooltip_start];

    assert_eq!(row_content_source.matches("Label::new").count(), 5);
    assert_eq!(row_content_source.matches(".selectable(false)").count(), 5);
}

#[test]
fn long_row_tooltip_is_bounded_by_row_and_overlay_limit() {
    assert_eq!(tooltip_width(180.0), 180.0);
    assert_eq!(tooltip_width(420.0), TOOLTIP_MAX_WIDTH);
    assert_eq!(tooltip_width(0.0), 1.0);
}

#[test]
fn compact_media_kind_column_preserves_large_queue_index_width() {
    egui::__run_test_ui(|ui| {
        let title_start = INDEX_WIDTH + MEDIA_KIND_WIDTH + ui.spacing().item_spacing.x * 2.0;

        assert_eq!(INDEX_WIDTH, 38.0);
        assert_eq!(MEDIA_KIND_WIDTH, 16.0);
        assert_eq!(title_start, 70.0);
    });
}

#[test]
fn every_playlist_media_kind_maps_to_an_explicit_artwork_glyph() {
    assert_eq!(
        media_kind_glyph(PlaylistMediaKind::Unknown),
        MediaKindGlyph::Unknown
    );
    assert_eq!(
        media_kind_glyph(PlaylistMediaKind::Audio),
        MediaKindGlyph::Audio
    );
    assert_eq!(
        media_kind_glyph(PlaylistMediaKind::Video),
        MediaKindGlyph::Video
    );
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
            super::renderer::show_rows(ui, &model, row_style(), &mut state, &mut output);
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
        ..PlaylistUiState::default()
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
        super::show(
            ui,
            Some(&model),
            &interaction,
            row_style(),
            &mut state,
            &mut output,
        );
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
            ..PlaylistUiState::default()
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
        ..PlaylistUiState::default()
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
    assert!(retrying_text.contains("Тип: Видео"));
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
    assert!(accessibility_text(8, &unavailable).contains("Тип: Медиа"));
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
