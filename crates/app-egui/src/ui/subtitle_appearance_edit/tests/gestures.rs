//! Жесты на настоящих кадрах egui: что делает мышь над видео и рамкой слоя.

use egui::{CursorIcon, Event, pos2};

use super::harness::{
    EditHarness, FrameReport, edits, layer_rect_px, picture_rect_px, total_move, total_scale,
    view_with, wheel,
};
use crate::state::AppearanceEdit;
use crate::ui::subtitle_appearance_edit::SubtitleAppearanceEditAction;

fn harness() -> EditHarness {
    EditHarness::new(view_with(Some(layer_rect_px())))
}

#[test]
fn dragging_inside_the_frame_moves_by_the_pointer_shift_in_picture_fractions() {
    let mut harness = harness();
    // Центр слоя (400, 470); тянем на 40 pt вправо и 20 pt вверх.
    let reports = harness.drag(&[pos2(400.0, 470.0), pos2(420.0, 460.0), pos2(440.0, 450.0)]);

    let (horizontal, vertical) = total_move(&edits(&reports));
    let picture = picture_rect_px();
    // Масштаб 1.0: 40 pt = 40 px из 1600 px; вверх на 20 pt = +20 px из 1000 px.
    assert!(
        (horizontal - 40.0 / picture.width).abs() < 1e-5,
        "{horizontal}"
    );
    assert!(
        (vertical - 20.0 / picture.height).abs() < 1e-5,
        "{vertical}"
    );
}

#[test]
fn drag_distance_follows_pixels_per_point() {
    let mut harness = harness();
    harness.ctx.set_pixels_per_point(2.0);
    // Слой в пикселях (300,450,200x40) при масштабе 2 стоит в точках на (150,225), 100x20.
    let reports = harness.drag(&[pos2(200.0, 235.0), pos2(220.0, 235.0)]);

    let (horizontal, vertical) = total_move(&edits(&reports));
    // 20 pt = 40 px из 1600 px картинки.
    assert!((horizontal - 40.0 / 1600.0).abs() < 1e-5, "{horizontal}");
    assert_eq!(vertical, 0.0);
}

#[test]
fn dragging_bare_video_outside_the_frame_changes_nothing() {
    let mut harness = harness();
    let reports = harness.drag(&[pos2(100.0, 200.0), pos2(300.0, 260.0)]);
    assert!(edits(&reports).is_empty(), "{:?}", edits(&reports));
}

#[test]
fn dragging_a_corner_outward_grows_the_text_and_inward_shrinks_it() {
    // Правый нижний угол рамки: слой (300..500, 450..490) плюс запас 4 pt.
    let corner = pos2(504.0, 494.0);
    let centre = pos2(400.0, 470.0);

    let mut growing = harness();
    let grow = edits(&growing.drag(&[corner, pos2(550.0, 520.0)]));
    let expected_grow = pos2(550.0, 520.0).distance(centre) / corner.distance(centre);
    let factor_grow = total_scale(&grow);
    assert!(factor_grow > 1.0, "{factor_grow}");
    assert!(
        (factor_grow - expected_grow).abs() < 0.01,
        "{factor_grow} vs {expected_grow}"
    );
    assert_eq!(
        total_move(&grow),
        (0.0, 0.0),
        "a corner drag must not move the layer"
    );

    let mut shrinking = harness();
    let shrink = edits(&shrinking.drag(&[corner, pos2(450.0, 480.0)]));
    assert!(total_scale(&shrink) < 1.0, "{}", total_scale(&shrink));
}

#[test]
fn wheel_over_the_video_scales_the_text_and_is_consumed() {
    let mut harness = harness();
    harness.frame(vec![Event::PointerMoved(pos2(100.0, 100.0))]);
    let report = harness.frame(wheel(40.0));

    assert_eq!(
        report.actions,
        vec![SubtitleAppearanceEditAction::Edit(
            AppearanceEdit::ScaleTextBy { factor: 1.1 }
        )]
    );
    assert_eq!(
        report.scroll_left, 0.0,
        "nobody else may scroll or change volume"
    );

    let down = harness.frame(wheel(-40.0));
    let factor = total_scale(&edits(&[down]));
    assert!(
        factor < 1.0 && (factor * 1.1 - 1.0).abs() < 1e-5,
        "{factor}"
    );
}

#[test]
fn wheel_works_over_the_frame_too() {
    let mut harness = harness();
    harness.frame(vec![Event::PointerMoved(pos2(400.0, 470.0))]);
    let report = harness.frame(wheel(40.0));
    assert!(total_scale(&edits(&[report])) > 1.0);
}

#[test]
fn wheel_outside_the_video_area_is_left_alone() {
    let mut harness = harness();
    // Над заголовком окна (выше свободной области).
    harness.frame(vec![Event::PointerMoved(pos2(100.0, 10.0))]);
    let report = harness.frame(wheel(40.0));
    assert!(edits(&[report]).is_empty());
}

#[test]
fn double_click_on_the_video_produces_no_actions() {
    let mut harness = harness();
    let mut reports = harness.click(pos2(100.0, 200.0));
    reports.extend(harness.click(pos2(100.0, 200.0)));
    assert!(
        reports.iter().all(|report| report.actions.is_empty()),
        "double-click must neither toggle fullscreen nor edit the style"
    );
}

#[test]
fn cursor_shows_grab_over_the_frame_resize_on_corners_and_default_elsewhere() {
    let mut harness = harness();
    assert_eq!(
        harness
            .frame(vec![Event::PointerMoved(pos2(400.0, 470.0))])
            .cursor,
        CursorIcon::Grab
    );
    assert_eq!(
        harness
            .frame(vec![Event::PointerMoved(pos2(504.0, 494.0))])
            .cursor,
        CursorIcon::ResizeNwSe
    );
    assert_eq!(
        harness
            .frame(vec![Event::PointerMoved(pos2(504.0, 446.0))])
            .cursor,
        CursorIcon::ResizeNeSw
    );
    assert_eq!(
        harness
            .frame(vec![Event::PointerMoved(pos2(100.0, 200.0))])
            .cursor,
        CursorIcon::Default
    );
}

#[test]
fn cursor_is_grabbing_while_the_layer_is_dragged() {
    let mut harness = harness();
    let reports = harness.drag(&[pos2(400.0, 470.0), pos2(430.0, 470.0), pos2(460.0, 470.0)]);
    // Кадры: наведение, нажатие, два движения, отпускание.
    assert_eq!(reports[3].cursor, CursorIcon::Grabbing);
}

#[test]
fn without_a_layer_there_is_no_frame_but_the_wheel_still_works() {
    let mut harness = EditHarness::new(view_with(None));
    let with_layer = EditHarness::new(view_with(Some(layer_rect_px()))).frame(Vec::new());
    let without_layer = harness.frame(Vec::new());
    assert!(
        without_layer.shapes.len() < with_layer.shapes.len(),
        "no dashes and handles without a layer"
    );

    // Перетаскивание там, где был бы слой, ничего не двигает.
    let reports = harness.drag(&[pos2(400.0, 470.0), pos2(440.0, 470.0)]);
    assert!(edits(&reports).is_empty());

    harness.frame(vec![Event::PointerMoved(pos2(100.0, 100.0))]);
    let report = harness.frame(wheel(40.0));
    assert!(total_scale(&edits(&[report])) > 1.0);
}

/// Действия всех кадров подряд.
fn all_actions(reports: &[FrameReport]) -> Vec<SubtitleAppearanceEditAction> {
    reports
        .iter()
        .flat_map(|report| report.actions.iter().copied())
        .collect()
}

#[test]
fn grabbing_the_frame_starts_the_move_once_before_any_shift() {
    let mut harness = harness();
    let reports = harness.drag(&[pos2(400.0, 470.0), pos2(420.0, 460.0), pos2(440.0, 450.0)]);
    let actions = all_actions(&reports);

    let starts: Vec<usize> = actions
        .iter()
        .enumerate()
        .filter(|(_, action)| **action == SubtitleAppearanceEditAction::StartMove)
        .map(|(index, _)| index)
        .collect();
    let first_shift = actions
        .iter()
        .position(|action| {
            matches!(
                action,
                SubtitleAppearanceEditAction::Edit(AppearanceEdit::MoveBy { .. })
            )
        })
        .expect("the drag moves the layer");
    assert_eq!(starts.len(), 1, "one grab per drag: {actions:?}");
    assert!(
        starts[0] < first_shift,
        "grab comes before the first shift: {actions:?}"
    );
}

#[test]
fn corner_drags_and_bare_video_drags_do_not_start_a_move() {
    let mut harness = harness();
    let mut reports = harness.drag(&[pos2(100.0, 200.0), pos2(300.0, 260.0)]);
    // Правый нижний угол рамки: слой (300..500, 450..490) плюс запас 4 pt.
    reports.extend(harness.drag(&[pos2(504.0, 494.0), pos2(540.0, 520.0)]));
    assert!(
        !all_actions(&reports).contains(&SubtitleAppearanceEditAction::StartMove),
        "only the frame body starts a move"
    );
}
