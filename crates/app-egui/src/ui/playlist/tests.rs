//! Focused characterization read-only virtualized Playlist UI.

use std::path::PathBuf;

use egui::{
    Event, Key, Modifiers, MouseWheelUnit, PointerButton, RawInput, Rect, TouchPhase, pos2, vec2,
};
use media_core::MediaDuration;
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, MoveItemIntent, PlaylistItemDraft, PlaylistItemId,
    PlaylistMediaKind, PlaylistQueue,
};
use ui_artwork_egui::MediaKindGlyph;

use super::renderer::{
    INDEX_WIDTH, MEDIA_KIND_WIDTH, ROW_HEIGHT, TOOLTIP_MAX_WIDTH, accessibility_text,
    anchored_scroll_offset, media_kind_glyph, row_fill, show_rows, stable_row_id, tooltip_width,
};
use super::status::{navigation_message, save_message};
use super::{PlaylistAction, PlaylistUiOutput, PlaylistUiState, ViewportAnchor};
use crate::playlist_runtime::{
    PlaylistGoCurrentTarget, PlaylistInteractionModel, PlaylistNavigationView, PlaylistSaveAttempt,
    PlaylistSaveView, PlaylistViewModel, PlaylistVisibleRow, PlaylistVisibleRowTestFixture,
};
use crate::ui::animation::UiMotion;
use crate::ui::skin::{MinimalSkin, PlayerSkin, PlaylistRowStyle, PlaylistToolbarStyle};

/// Focused UI tests используют те же explicit row tokens, что production MinimalSkin.
fn row_style() -> PlaylistRowStyle {
    MinimalSkin.playlist_row_style()
}

/// Toolbar tests используют production tokens MinimalSkin без локальных цветов.
fn toolbar_style() -> PlaylistToolbarStyle {
    MinimalSkin.playlist_toolbar_style()
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
    PlaylistViewModel::for_queue_with_revision(queue, structural_revision)
}

/// Строит read model с подтверждённым active ID и без pending/controller shortcuts.
fn model_with_active(
    queue: &PlaylistQueue,
    structural_revision: u64,
    active_row_index: Option<usize>,
) -> PlaylistViewModel {
    // Индекс fixture разрешается через canonical queue Item ID.
    let active_item_id = active_row_index.map(|row_index| playable_item_id_at(queue, row_index));
    // Test-only view boundary меняет только ActiveMediaIdentity.
    PlaylistViewModel::for_queue_with_active_item_for_test(
        queue,
        structural_revision,
        active_item_id,
    )
}

/// Возвращает stable ID строки fixture, не раскрывая slice/index queue storage.
fn playable_item_id_at(queue: &PlaylistQueue, row_index: usize) -> PlaylistItemId {
    queue
        .iter_playable_ids()
        .nth(row_index)
        .expect("fixture должен содержать playable строку по заданной позиции")
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

/// Строит один trackpad/wheel кадр в screen-point координатах egui.
fn wheel_scroll(delta_y: f32) -> Event {
    // Positive delta двигает content вниз согласно egui Event contract.
    Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: vec2(0.0, delta_y),
        phase: TouchPhase::Move,
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
    // Обычные interaction tests используют стандартную policy.
    render_playlist_input_with_motion(context, model, state, UiMotion::Standard, input)
}

/// Рендерит production playlist rows с явно выбранной motion policy.
fn render_playlist_input_with_motion(
    context: &egui::Context,
    model: &PlaylistViewModel,
    state: &mut PlaylistUiState,
    motion: UiMotion,
    input: RawInput,
) -> Vec<PlaylistAction> {
    let mut output = PlaylistUiOutput::default();
    let _ = context.run_ui(input, |ui| {
        // Фиксированный размер делает координаты колонок стабильными между input frames.
        ui.set_width(420.0);
        ui.set_height(120.0);
        show_rows(ui, model, row_style(), motion, state, &mut output);
    });
    output.take_actions()
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

    // Index, title, duration и error остаются четырьмя невыделяемыми text labels.
    assert_eq!(row_content_source.matches("Label::new").count(), 4);
    // Каждый оставшийся row label явно запрещает системное выделение текста.
    assert_eq!(row_content_source.matches(".selectable(false)").count(), 4);
    // У активной строки больше нет отдельного декоративного Play-маркера.
    assert!(!row_content_source.contains("RichText::new(\"▶\")"));
    // Renderer не должен возвращать удалённый векторный глиф через overlay.
    assert!(!renderer_source.contains("active_track_glyph"));
    // Обычный заголовок больше не получает forced strong foreground.
    assert!(!row_content_source.contains("row.display_title()).strong()"));
    // Контрастный foreground применяется только к authoritative active row.
    assert!(row_content_source.contains("if row.is_active()"));
    assert!(row_content_source.contains("title_text.color(row_style.active_title_color)"));
    // Playback marker проходит через neutral artwork facade, а не text widget.
    assert!(renderer_source.contains("artwork.playlist_row_marker"));
}

#[test]
fn active_and_selection_keep_independent_row_visual_channels() {
    // Оба состояния принадлежат одному fixture, чтобы проверить их совместную композицию.
    let active_selected_row =
        PlaylistVisibleRow::from_test_fixture(PlaylistVisibleRowTestFixture {
            item_id: PlaylistItemId::from_persistence_value(41).unwrap(),
            fallback_display_name: "active-selected.mkv".into(),
            display_title: "Активная и выделенная строка".into(),
            duration: Some(MediaDuration::from_secs(90)),
            media_kind: PlaylistMediaKind::Video,
            active: true,
            pending: false,
            selected: true,
            safe_error_summary: None,
        });
    // Selection surface не исчезает из-за отдельного active marker-а.
    assert_eq!(
        row_fill(row_style(), &active_selected_row, false),
        row_style().selected_fill
    );
    // Hover усиливает именно selection surface, не подменяя playback semantics.
    assert_eq!(
        row_fill(row_style(), &active_selected_row, true),
        row_style().selected_hover_fill
    );

    // Active-only fixture подтверждает, что playback не маскируется selection fill-ом.
    let active_only_row = PlaylistVisibleRow::from_test_fixture(PlaylistVisibleRowTestFixture {
        item_id: PlaylistItemId::from_persistence_value(42).unwrap(),
        fallback_display_name: "active-only.mkv".into(),
        display_title: "Только активная строка".into(),
        duration: Some(MediaDuration::from_secs(120)),
        media_kind: PlaylistMediaKind::Video,
        active: true,
        pending: false,
        selected: false,
        safe_error_summary: None,
    });
    // Прозрачная row surface оставляет active fill/marker отдельному moving layer.
    assert_eq!(
        row_fill(row_style(), &active_only_row, false),
        egui::Color32::TRANSPARENT
    );
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
            super::renderer::show_rows(
                ui,
                &model,
                row_style(),
                UiMotion::Standard,
                &mut state,
                &mut output,
            );
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
fn confirmed_active_item_id_drives_visible_transition_and_stops_repainting_at_target() {
    // Десять строк оставляют первые соседние цели одновременно видимыми.
    let queue = queue(10);
    // Первый model подтверждает Item ID нулевой строки.
    let first_model = model_with_active(&queue, 1, Some(0));
    // Второй model меняет только authoritative ActiveMediaIdentity.
    let second_model = model_with_active(&queue, 1, Some(1));
    // Отдельный static render даёт точный target rect без копирования renderer math в test.
    let expected_context = egui::Context::default();
    // Static state впервые видит второй active ID и поэтому не анимирует его.
    let mut expected_state = PlaylistUiState::default();
    // Первый authoritative frame мгновенно привязывает accent к target row.
    render_playlist_input(
        &expected_context,
        &second_model,
        &mut expected_state,
        playlist_raw_input(Vec::new(), 0.0),
    );
    // Exact target rect читается только через focused test getter.
    let expected_target_rect = expected_state
        .active_accent
        .current_rect_for_test()
        .expect("second row is visible");

    // Production sequence использует один Context и один renderer-owned state.
    let context = egui::Context::default();
    // State не содержит controller или playback lifecycle.
    let mut state = PlaylistUiState::default();
    // Первый None→Some не запускает движение.
    render_playlist_input(
        &context,
        &first_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.0),
    );
    // Source rect соответствует первой активной строке.
    let source_rect = state
        .active_accent
        .current_rect_for_test()
        .expect("first row is visible");
    // Idle authoritative frame не просит repaint.
    assert!(!state.active_accent.needs_repaint());

    // Подтверждённый Some(old)→Some(new) стартует nearby transition.
    render_playlist_input(
        &context,
        &second_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.01),
    );
    // Стартовый кадр остаётся в source rect без one-frame jump.
    assert_eq!(
        state.active_accent.current_rect_for_test(),
        Some(source_rect)
    );
    // Незавершённый transition просит следующий кадр.
    assert!(state.active_accent.needs_repaint());

    // Следующий кадр продвигает cubic sample внутрь 220-ms траектории.
    render_playlist_input(
        &context,
        &second_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.12),
    );
    // Mid-flight rect уже покинул source.
    let intermediate_rect = state
        .active_accent
        .current_rect_for_test()
        .expect("nearby accent remains visible");
    // Mid-flight rect ещё не обязан совпадать с target.
    assert_ne!(intermediate_rect, source_rect);
    // Движение продолжается до полного duration.
    assert!(state.active_accent.needs_repaint());

    // Несколько bounded dt кадров суммарно проходят полный 220-ms контракт.
    for time in [0.23, 0.26] {
        // Каждый render использует тот же authoritative model.
        render_playlist_input(
            &context,
            &second_model,
            &mut state,
            playlist_raw_input(Vec::new(), time),
        );
    }
    // Завершённый accent точно привязан к authoritative target row.
    assert_eq!(
        state.active_accent.current_rect_for_test(),
        Some(expected_target_rect)
    );
    // После завершения idle repaint отсутствует.
    assert!(!state.active_accent.needs_repaint());
}

#[test]
fn offscreen_active_change_follows_without_traversing_full_queue() {
    // Большая очередь проверяет virtualized follow path.
    let queue = queue(10_000);
    // Сначала подтверждённо играет видимая первая строка.
    let first_model = model_with_active(&queue, 1, Some(0));
    // Затем authoritative target переносится в самый конец.
    let last_model = model_with_active(&queue, 1, Some(9_999));
    // Один Context сохраняет ScrollArea offset между кадрами.
    let context = egui::Context::default();
    // UI state хранит только ephemeral geometry.
    let mut state = PlaylistUiState::default();
    // Начальный кадр привязывает видимый source.
    render_playlist_input(
        &context,
        &first_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.0),
    );

    // Output второго кадра позволяет проверить bounded visible hint.
    let mut output = PlaylistUiOutput::default();
    // Some→Some с off-screen target стартует 360-ms follow.
    let _ = context.run_ui(playlist_raw_input(Vec::new(), 0.01), |ui| {
        // Production viewport совпадает с обычным headless helper.
        ui.set_width(420.0);
        // Небольшая высота гарантирует off-screen target.
        ui.set_height(120.0);
        // Реальный renderer остаётся единственным paint/interaction path.
        show_rows(
            ui,
            &last_model,
            row_style(),
            UiMotion::Standard,
            &mut state,
            &mut output,
        );
    });
    // State различает follow от nearby без раскрытия timeline storage.
    assert!(state.active_accent.is_following_for_test());
    // Даже 10k очередь публикует только bounded visible rows.
    assert!(output.visible_item_ids.len() <= super::MAX_VISIBLE_HINT_ITEMS);
    // Полного traversal через visible output не произошло.
    assert!(output.visible_item_ids.len() < queue.top_level_entry_count());

    // Следующий кадр продвигает auto-scroll по cubic curve.
    render_playlist_input(
        &context,
        &last_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.11),
    );
    // Authoritative ScrollArea offset уже сдвинулся к цели.
    assert!(
        state
            .active_accent
            .scroll_offset_for_test()
            .is_some_and(|offset| offset > 0.0)
    );
    // Edge-hold accent остаётся видимым во время основной прокрутки.
    assert!(state.active_accent.current_rect_for_test().is_some());
}

#[test]
fn viewing_another_queue_region_prevents_future_viewport_takeover() {
    // Очередь достаточно длинная для независимого viewport region.
    let queue = queue(100);
    // Первая строка является подтверждённой active identity.
    let first_model = model_with_active(&queue, 1, Some(0));
    // Следующий authoritative target находится далеко от ручного viewport-а.
    let later_model = model_with_active(&queue, 1, Some(70));
    // Context хранит persistent ScrollArea state.
    let context = egui::Context::default();
    // UI state начинает без viewport ownership.
    let mut state = PlaylistUiState::default();
    // Начальный active accent видим.
    render_playlist_input(
        &context,
        &first_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.0),
    );

    // Explicit Go Current на другую строку моделирует осознанный viewport intent пользователя.
    state.request_go_current(PlaylistGoCurrentTarget::Row(playable_item_id_at(
        &queue, 50,
    )));
    // Существующий focus path центрирует строку и отменяет auto-follow.
    render_playlist_input(
        &context,
        &first_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.01),
    );
    // Старая active row теперь вне экрана, поэтому decorative rect отсутствует.
    assert!(state.active_accent.current_rect_for_test().is_none());

    // Новая подтверждённая цель не имеет видимого source accent.
    render_playlist_input(
        &context,
        &later_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.02),
    );
    // Renderer не отбирает viewport у выбранного пользователем региона.
    assert!(!state.active_accent.is_following_for_test());
    // Без transition нет explicit repaint loop.
    assert!(!state.active_accent.needs_repaint());
}

#[test]
fn wheel_input_cancels_active_auto_follow_immediately() {
    // Off-screen target гарантированно создаёт follow до wheel event.
    let queue = queue(100);
    // Видимая source identity.
    let first_model = model_with_active(&queue, 1, Some(0));
    // Далёкая target identity.
    let target_model = model_with_active(&queue, 1, Some(80));
    // Persistent headless context нужен для input attribution viewport-у.
    let context = egui::Context::default();
    // UI-owned state не мутирует queue.
    let mut state = PlaylistUiState::default();
    // Первый кадр устанавливает source geometry.
    render_playlist_input(
        &context,
        &first_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.0),
    );
    // Второй кадр начинает auto-follow.
    render_playlist_input(
        &context,
        &target_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.01),
    );
    // До ручного input transition действительно follow.
    assert!(state.active_accent.is_following_for_test());

    // Pointer и wheel находятся внутри последнего authoritative viewport-а.
    let events = vec![Event::PointerMoved(pos2(200.0, 60.0)), wheel_scroll(-36.0)];
    // Wheel кадр должен принадлежать ScrollArea, а не соседнему UI.
    render_playlist_input(
        &context,
        &target_model,
        &mut state,
        playlist_raw_input(events, 0.02),
    );
    // User input немедленно отменяет auto-follow.
    assert!(!state.active_accent.is_following_for_test());
    // Отменённый transition не оставляет repaint.
    assert!(!state.active_accent.needs_repaint());
}

#[test]
fn stop_and_structural_change_never_reuse_stale_accent_geometry() {
    // Соседние строки позволяют сначала запустить nearby motion.
    let queue = queue(8);
    // Начальная active identity.
    let first_model = model_with_active(&queue, 1, Some(0));
    // Новый active ID на той же structural revision запускает transition.
    let second_model = model_with_active(&queue, 1, Some(1));
    // Та же active identity на новой revision моделирует structural mutation.
    let structurally_changed_model = model_with_active(&queue, 2, Some(1));
    // Stop очищает ActiveMediaIdentity.
    let stopped_model = model_with_active(&queue, 2, None);
    // Один Context сохраняет renderer state.
    let context = egui::Context::default();
    // Эфемерный state начинается пустым.
    let mut state = PlaylistUiState::default();
    // Первый ID привязывается мгновенно.
    render_playlist_input(
        &context,
        &first_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.0),
    );
    // Some→Some начинает nearby transition.
    render_playlist_input(
        &context,
        &second_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.01),
    );
    // До structural change repaint активен.
    assert!(state.active_accent.needs_repaint());

    // Новая structural revision инвалидирует source/index geometry.
    render_playlist_input(
        &context,
        &structurally_changed_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.02),
    );
    // Accent мгновенно перепривязан к актуальному target rect.
    assert!(state.active_accent.current_rect_for_test().is_some());
    // Stale transition больше не запрашивает repaint.
    assert!(!state.active_accent.needs_repaint());

    // Explicit Stop удаляет authoritative target.
    render_playlist_input(
        &context,
        &stopped_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.03),
    );
    // Старый accent не получает exit animation.
    assert!(state.active_accent.current_rect_for_test().is_none());
    // Stop не запускает repaint.
    assert!(!state.active_accent.needs_repaint());
}

#[test]
fn reduced_motion_applies_required_offscreen_scroll_in_one_frame() {
    // Длинная очередь гарантирует необходимость viewport reposition.
    let queue = queue(100);
    // Источник видим в первом кадре.
    let first_model = model_with_active(&queue, 1, Some(0));
    // Target находится далеко за viewport-ом.
    let target_model = model_with_active(&queue, 1, Some(90));
    // Persistent context сохраняет scroll state.
    let context = egui::Context::default();
    // UI state не персистится.
    let mut state = PlaylistUiState::default();
    // Initial active paint остаётся мгновенным в любой policy.
    render_playlist_input_with_motion(
        &context,
        &first_model,
        &mut state,
        UiMotion::Reduced,
        playlist_raw_input(Vec::new(), 0.0),
    );
    // Some→Some применяет конечный follow offset сразу.
    render_playlist_input_with_motion(
        &context,
        &target_model,
        &mut state,
        UiMotion::Reduced,
        playlist_raw_input(Vec::new(), 0.01),
    );
    // Target row попадает в viewport уже в этом render pass.
    assert!(state.active_accent.current_rect_for_test().is_some());
    // Необходимый scroll offset применён без intermediate кадров.
    assert!(
        state
            .active_accent
            .scroll_offset_for_test()
            .is_some_and(|offset| offset > 0.0)
    );
    // Reduced motion не оставляет repaint loop.
    assert!(!state.active_accent.needs_repaint());
}

#[test]
fn unchanged_confirmed_identity_does_not_restart_for_non_structural_updates() {
    // Pending/error presentation revisions не входят в active_item_id boundary.
    let queue = queue(6);
    // Оба read model fixture имеют один и тот же подтверждённый Item ID.
    let active_model = model_with_active(&queue, 4, Some(2));
    // Intent-method возвращает только committed identity.
    assert_eq!(
        active_model.active_item_id(),
        Some(playable_item_id_at(&queue, 2))
    );
    // Context/state моделируют повторный frame после любого non-structural update.
    let context = egui::Context::default();
    // UI state изначально не наблюдал identity.
    let mut state = PlaylistUiState::default();
    // Первый frame мгновенно привязывает accent.
    render_playlist_input(
        &context,
        &active_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.0),
    );
    // Exact rect нужен для проверки отсутствия перезапуска.
    let first_rect = state.active_accent.current_rect_for_test();
    // Неизменившийся active ID не создаёт новую motion timeline.
    render_playlist_input(
        &context,
        &active_model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.1),
    );
    // Decorative rect остаётся привязанным к той же row.
    assert_eq!(state.active_accent.current_rect_for_test(), first_rect);
    // Pending/error-like non-structural frame не просит repaint.
    assert!(!state.active_accent.needs_repaint());
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
    // Authoritative model содержит видимый active accent.
    let model = model_with_active(&queue, 4, Some(0));
    let top_item_id = playable_item_id_at(&queue, 33);
    // Сначала production-enabled pass создаёт реальную ephemeral accent geometry.
    let context = egui::Context::default();
    // State принадлежит authoritative Playlist content, а не sidebar transition copy.
    let mut state = PlaylistUiState::default();
    // Initial active ID привязывается без motion.
    render_playlist_input(
        &context,
        &model,
        &mut state,
        playlist_raw_input(Vec::new(), 0.0),
    );
    // Accent snapshot должен пережить disabled visual copy неизменным.
    let accent_rect_before = state.active_accent.current_rect_for_test();
    // Existing viewport invariant настраивается после bootstrap render.
    state.viewport_anchor = Some(ViewportAnchor {
        item_id: top_item_id,
        intra_row_offset: 6.0,
    });
    // Structural observation также не должно заменяться visual copy.
    state.observed_structural_revision = Some(model.structural_revision());
    // Go-current intent отсутствует в этом regression.
    state.go_current = None;
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
            super::PlaylistShowInput {
                model: Some(&model),
                interaction: &interaction,
                row_style: row_style(),
                toolbar_style: toolbar_style(),
                motion: UiMotion::Standard,
            },
            &mut state,
            &mut output,
        );
    });

    assert_eq!(state.viewport_anchor.unwrap().item_id, top_item_id);
    // Disabled sidebar copy не мутирует authoritative active animation state.
    assert_eq!(
        state.active_accent.current_rect_for_test(),
        accent_rect_before
    );
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
        let top_item_id = playable_item_id_at(&queue, 8);
        let inside_anchor = playable_item_id_at(&queue, 12);
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
                queue.move_item(
                    playlist_core::PlaylistEntryId::Single(inserted_item_id),
                    MoveItemIntent::Before(playlist_core::PlaylistEntryId::Single(top_item_id)),
                );
            }
            InsertionPlacement::Inside => {
                queue.move_item(
                    playlist_core::PlaylistEntryId::Single(inserted_item_id),
                    MoveItemIntent::After(playlist_core::PlaylistEntryId::Single(inside_anchor)),
                );
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
    let top_item_id = playable_item_id_at(&queue, 11);
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

    let (navigation, _) = navigation_message(PlaylistNavigationView::AwaitingUserAfterFailure {
        item_id,
        origin_already_ended: false,
    })
    .expect("D55 status");
    assert!(navigation.contains("Автоматический переход остановлен"));
    assert!(navigation.contains("Next, Previous или повтор"));
}

#[test]
fn routine_background_save_stays_silent() {
    // Переключение трека обновляет сохранённый идентификатор текущего элемента без уведомления.
    assert_eq!(save_message(PlaylistSaveView::Saving), None);
}

#[test]
fn save_warning_exposes_retry_and_unavailable_row_stays_non_modal() {
    let (warning, _) = save_message(PlaylistSaveView::WarningRetryAvailable {
        attempt: PlaylistSaveAttempt::for_test(3),
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
