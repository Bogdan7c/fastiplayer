//! Двойной клик по области видео переключает фуллскрин, одиночный — ничего не делает.

use super::*;
use crate::ui::test_frame::{click_frames, run_ui_frame};

/// Прогоняет кадры кликов и собирает действия области видео со всех кадров.
fn actions_after_clicks(
    surface_rect: Rect,
    click_position: egui::Pos2,
    click_count: usize,
) -> Vec<VideoSurfaceAction> {
    let egui_ctx = egui::Context::default();
    let mut actions = Vec::new();
    for frame_input in click_frames(click_position, click_count) {
        run_ui_frame(&egui_ctx, frame_input, |ui| {
            actions.extend(show(ui, surface_rect));
        });
    }
    actions
}

fn surface_rect() -> Rect {
    Rect::from_min_max(egui::pos2(0.0, 40.0), egui::pos2(800.0, 500.0))
}

#[test]
fn double_click_on_video_requests_fullscreen_toggle_once() {
    let actions = actions_after_clicks(surface_rect(), egui::pos2(400.0, 300.0), 2);

    assert_eq!(actions, vec![VideoSurfaceAction::ToggleFullscreen]);
}

#[test]
fn single_click_on_video_does_nothing() {
    let actions = actions_after_clicks(surface_rect(), egui::pos2(400.0, 300.0), 1);

    assert!(actions.is_empty(), "одиночный клик не должен ничего делать");
}

#[test]
fn double_click_outside_video_area_is_ignored() {
    // Точка в зоне нижней панели (ниже области видео).
    let actions = actions_after_clicks(surface_rect(), egui::pos2(400.0, 560.0), 2);

    assert!(actions.is_empty());
}

#[test]
fn empty_video_area_never_produces_actions() {
    let collapsed_rect = Rect::from_min_size(egui::pos2(100.0, 100.0), egui::Vec2::ZERO);

    let actions = actions_after_clicks(collapsed_rect, egui::pos2(100.0, 100.0), 2);

    assert!(actions.is_empty());
}
