//! Сдвиг панелей: раскладка без сдвига совпадает с обычной панелью, уехавшая часть
//! освобождает место и не принимает клики.

use super::*;
use crate::ui::test_frame::{click_frames, input_at, run_ui_frame};

const TITLEBAR_HEIGHT: f32 = 40.0;
const BOTTOM_CONTENT_HEIGHT: f32 = 60.0;

fn full_window_rect() -> Rect {
    Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))
}

fn top_panel(ui: &mut Ui) -> bool {
    egui::Panel::top("test_titlebar")
        .exact_size(TITLEBAR_HEIGHT)
        .show(ui, |ui| ui.button("Кнопка заголовка").clicked())
        .inner
}

fn bottom_panel(ui: &mut Ui) {
    egui::Panel::bottom("test_bottom_controls").show(ui, |ui| {
        ui.allocate_space(egui::vec2(100.0, BOTTOM_CONTENT_HEIGHT));
    });
}

/// Что увидел кадр со сдвигаемыми панелями.
#[derive(Debug)]
struct SlidFrame {
    remaining_rect: Rect,
    top_visible: Option<Rect>,
    bottom_visible: Option<Rect>,
    top_button_clicked: bool,
}

fn run_slid_frame(
    egui_ctx: &egui::Context,
    input: egui::RawInput,
    hidden_fraction: f32,
) -> SlidFrame {
    let mut frame = SlidFrame {
        remaining_rect: Rect::NOTHING,
        top_visible: None,
        bottom_visible: None,
        top_button_clicked: false,
    };
    run_ui_frame(egui_ctx, input, |ui| {
        let top = show(
            ui,
            EdgeSlide {
                id: egui::Id::new("test_top_slide"),
                edge: ScreenEdge::Top,
                hidden_fraction,
            },
            top_panel,
        );
        let bottom = show(
            ui,
            EdgeSlide {
                id: egui::Id::new("test_bottom_slide"),
                edge: ScreenEdge::Bottom,
                hidden_fraction,
            },
            bottom_panel,
        );
        frame = SlidFrame {
            remaining_rect: ui.available_rect_before_wrap(),
            top_visible: top.visible_rect,
            bottom_visible: bottom.visible_rect,
            top_button_clicked: top.inner,
        };
    });
    frame
}

/// Остаток окна, когда те же панели нарисованы напрямую, как до сессии UX 14.
fn remaining_rect_with_direct_panels() -> Rect {
    let egui_ctx = egui::Context::default();
    let mut remaining_rect = Rect::NOTHING;
    for frame_index in 0..3 {
        run_ui_frame(
            &egui_ctx,
            input_at(f64::from(frame_index) * 0.016, Vec::new()),
            |ui| {
                top_panel(ui);
                bottom_panel(ui);
                remaining_rect = ui.available_rect_before_wrap();
            },
        );
    }
    remaining_rect
}

/// Прогоняет несколько кадров с fraction 0, чтобы высоты панелей были измерены.
fn measured_context() -> egui::Context {
    let egui_ctx = egui::Context::default();
    for frame_index in 0..3 {
        run_slid_frame(
            &egui_ctx,
            input_at(f64::from(frame_index) * 0.016, Vec::new()),
            0.0,
        );
    }
    egui_ctx
}

#[test]
fn visible_panels_leave_exactly_the_same_layout_as_direct_panels() {
    let egui_ctx = measured_context();

    let frame = run_slid_frame(&egui_ctx, input_at(1.0, Vec::new()), 0.0);

    assert_eq!(frame.remaining_rect, remaining_rect_with_direct_panels());
    let top_visible = frame.top_visible.expect("заголовок виден");
    assert_eq!(top_visible.height(), TITLEBAR_HEIGHT);
    let bottom_visible = frame.bottom_visible.expect("нижняя панель видна");
    assert_eq!(bottom_visible.bottom(), full_window_rect().bottom());
    assert!(bottom_visible.height() >= BOTTOM_CONTENT_HEIGHT);
}

#[test]
fn fully_hidden_panels_free_the_whole_window() {
    let egui_ctx = measured_context();

    let frame = run_slid_frame(&egui_ctx, input_at(1.0, Vec::new()), 1.0);

    assert_eq!(frame.remaining_rect, full_window_rect());
    assert_eq!(frame.top_visible, None);
    assert_eq!(frame.bottom_visible, None);
}

#[test]
fn half_hidden_panels_show_and_reserve_half_of_their_height() {
    let egui_ctx = measured_context();
    let bottom_height = run_slid_frame(&egui_ctx, input_at(0.9, Vec::new()), 0.0)
        .bottom_visible
        .expect("нижняя панель видна")
        .height();

    let frame = run_slid_frame(&egui_ctx, input_at(1.0, Vec::new()), 0.5);

    let top_visible = frame.top_visible.expect("половина заголовка видна");
    assert_eq!(top_visible.top(), 0.0);
    assert_eq!(top_visible.height(), TITLEBAR_HEIGHT * 0.5);
    let bottom_visible = frame.bottom_visible.expect("половина нижней панели видна");
    assert!((bottom_visible.height() - bottom_height * 0.5).abs() < 1.0);
    assert_eq!(frame.remaining_rect.top(), TITLEBAR_HEIGHT * 0.5);
    assert_eq!(frame.remaining_rect.bottom(), bottom_visible.top());
}

#[test]
fn hidden_panel_buttons_do_not_receive_clicks() {
    // Кнопка заголовка в видимом состоянии: верхний левый угол панели.
    let button_position = egui::pos2(20.0, 10.0);

    let visible_ctx = measured_context();
    let clicked_when_visible = click_frames(button_position, 1)
        .into_iter()
        .any(|input| run_slid_frame(&visible_ctx, input, 0.0).top_button_clicked);
    assert!(clicked_when_visible, "видимая кнопка обязана нажиматься");

    let hidden_ctx = measured_context();
    let clicked_when_hidden = click_frames(button_position, 1)
        .into_iter()
        .any(|input| run_slid_frame(&hidden_ctx, input, 1.0).top_button_clicked);
    assert!(!clicked_when_hidden, "уехавшая кнопка не должна нажиматься");
}

#[test]
fn invalid_fraction_is_treated_as_visible() {
    let egui_ctx = measured_context();

    let frame = run_slid_frame(&egui_ctx, input_at(1.0, Vec::new()), f32::NAN);

    assert_eq!(frame.remaining_rect, remaining_rect_with_direct_panels());
}
