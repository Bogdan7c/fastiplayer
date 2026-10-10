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

// --- Колесо над видео меняет громкость (UX edge cases 2, B3) ---

use egui::{Event, Modifiers, MouseWheelUnit, TouchPhase, vec2};

fn wheel_events(vertical_points: f32) -> Vec<Event> {
    let wheel = |phase, delta_y| Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: vec2(0.0, delta_y),
        phase,
        modifiers: Modifiers::NONE,
    };
    vec![
        wheel(TouchPhase::Start, 0.0),
        wheel(TouchPhase::Move, vertical_points),
    ]
}

/// Прогоняет «наведение» и кадр с колесом; `sidebar_rect` — зона, куда видео не входит
/// (плейлист/сайдбар со своей прокруткой, которая читает колесо в том же кадре).
fn actions_after_wheel(
    pointer_position: egui::Pos2,
    wheel_points: f32,
) -> (Vec<VideoSurfaceAction>, f32) {
    let egui_ctx = egui::Context::default();
    let sidebar_rect = Rect::from_min_max(egui::pos2(0.0, 40.0), egui::pos2(200.0, 500.0));
    let video_rect = Rect::from_min_max(egui::pos2(200.0, 40.0), egui::pos2(800.0, 500.0));
    let mut actions = Vec::new();
    let mut scroll_left_for_others = 0.0;
    for events in [
        vec![Event::PointerMoved(pointer_position)],
        [
            vec![Event::PointerMoved(pointer_position)],
            wheel_events(wheel_points),
        ]
        .concat(),
    ] {
        actions.clear();
        run_ui_frame(
            &egui_ctx,
            crate::ui::test_frame::input_at(0.0, events),
            |ui| {
                let _sidebar = ui.interact(sidebar_rect, ui.id().with("sidebar"), Sense::hover());
                actions.extend(show(ui, video_rect));
                scroll_left_for_others = ui.input(|input| input.smooth_scroll_delta().y);
            },
        );
    }
    (actions, scroll_left_for_others)
}

#[test]
fn wheel_up_over_video_requests_one_louder_step_and_consumes_the_scroll() {
    let (actions, scroll_left_for_others) = actions_after_wheel(egui::pos2(500.0, 300.0), 40.0);

    assert_eq!(actions, vec![VideoSurfaceAction::AdjustVolumeSteps(1)]);
    assert_eq!(
        scroll_left_for_others, 0.0,
        "прокрутка не должна уходить дальше"
    );
}

#[test]
fn wheel_down_over_video_requests_a_quieter_step() {
    let (actions, _) = actions_after_wheel(egui::pos2(500.0, 300.0), -80.0);

    assert_eq!(actions, vec![VideoSurfaceAction::AdjustVolumeSteps(-2)]);
}

#[test]
fn wheel_over_sidebar_area_does_not_change_volume_and_stays_scroll() {
    let (actions, scroll_left_for_others) = actions_after_wheel(egui::pos2(100.0, 300.0), 40.0);

    assert!(
        actions.is_empty(),
        "над плейлистом колесо не громкость: {actions:?}"
    );
    assert!(
        scroll_left_for_others > 0.0,
        "прокрутка плейлиста сохранена"
    );
}

#[test]
fn small_wheel_deltas_accumulate_until_a_whole_step() {
    let mut accumulator = wheel::WheelAccumulator::default();

    assert_eq!(accumulator.take_steps(15.0), 0);
    assert_eq!(accumulator.take_steps(15.0), 0);
    assert_eq!(
        accumulator.take_steps(15.0),
        1,
        "45 points = шаг + остаток 5"
    );
    assert_eq!(accumulator.take_steps(-45.0), -1, "остаток 5 - 45 = -40");
}
