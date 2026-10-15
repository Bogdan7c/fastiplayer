//! Владение вводом по видео: в просмотре работают двойной клик и колесо плеера, в режиме
//! правки субтитров - только жесты правки.

use egui::{Event, Modifiers, MouseWheelUnit, PointerButton, Rect, TouchPhase, pos2, vec2};

use super::{VideoAreaMode, VideoAreaOutput, show};
use crate::state::{AppearanceEdit, AppearanceEditView, UserSubtitleStyle};
use crate::ui::player_controls::ControlAction;
use crate::ui::skin::{MinimalSkin, PlayerSkin};
use crate::ui::subtitle_appearance_edit::{
    SubtitleAppearanceEditAction, SubtitleAppearanceEditInput,
};
use crate::ui::test_frame::{app_behavior_context, input_at, run_ui_frame};

fn video_rect() -> Rect {
    Rect::from_min_max(pos2(0.0, 40.0), pos2(800.0, 560.0))
}

fn view() -> AppearanceEditView {
    AppearanceEditView {
        draft: UserSubtitleStyle::default(),
        layer_rect: None,
        picture_rect: None,
    }
}

/// Прогоняет кадры в выбранном режиме; возвращает объединённые намерения.
fn run(editing: bool, frames: Vec<Vec<Event>>) -> VideoAreaOutput {
    let ctx = app_behavior_context();
    let style = MinimalSkin.subtitle_appearance_edit_style();
    let edit_view = view();
    let mut total = VideoAreaOutput::default();
    // Два пустых кадра: egui узнаёт прямоугольники слоёв после кадра.
    let all_frames = [Vec::new(), Vec::new()].into_iter().chain(frames);
    for (index, events) in all_frames.enumerate() {
        let _ = run_ui_frame(&ctx, input_at(index as f64 * 0.05, events), |ui| {
            let mode = if editing {
                VideoAreaMode::SubtitleAppearanceEdit(SubtitleAppearanceEditInput {
                    view: &edit_view,
                    video_rect: video_rect(),
                    style: &style,
                })
            } else {
                VideoAreaMode::Playback
            };
            let output = show(ui, video_rect(), mode);
            total.control_actions.extend(output.control_actions);
            total
                .subtitle_edit_actions
                .extend(output.subtitle_edit_actions);
        });
    }
    total
}

fn click(position: egui::Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos: position,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn double_click_frames() -> Vec<Vec<Event>> {
    let point = pos2(100.0, 100.0);
    vec![
        vec![Event::PointerMoved(point)],
        vec![click(point, true)],
        vec![click(point, false)],
        vec![click(point, true)],
        vec![click(point, false)],
    ]
}

fn wheel_frames() -> Vec<Vec<Event>> {
    let wheel = |phase, delta_y| Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: vec2(0.0, delta_y),
        phase,
        modifiers: Modifiers::NONE,
    };
    vec![
        vec![Event::PointerMoved(pos2(100.0, 100.0))],
        vec![wheel(TouchPhase::Start, 0.0), wheel(TouchPhase::Move, 40.0)],
    ]
}

#[test]
fn playback_mode_double_click_toggles_fullscreen_and_wheel_changes_volume() {
    let double_click = run(false, double_click_frames());
    assert_eq!(
        double_click.control_actions,
        vec![ControlAction::ToggleFullscreen]
    );

    let wheel = run(false, wheel_frames());
    assert_eq!(
        wheel.control_actions,
        vec![ControlAction::AdjustVolumeSteps(1)]
    );
    assert!(wheel.subtitle_edit_actions.is_empty());
}

#[test]
fn edit_mode_double_click_does_not_toggle_fullscreen() {
    let output = run(true, double_click_frames());
    assert!(
        output.control_actions.is_empty(),
        "{:?}",
        output.control_actions
    );
    assert!(output.subtitle_edit_actions.is_empty());
}

#[test]
fn edit_mode_wheel_resizes_the_text_and_never_changes_volume() {
    let output = run(true, wheel_frames());
    assert!(
        output.control_actions.is_empty(),
        "{:?}",
        output.control_actions
    );
    assert_eq!(
        output.subtitle_edit_actions,
        vec![SubtitleAppearanceEditAction::Edit(
            AppearanceEdit::ScaleTextBy { factor: 1.1 }
        )]
    );
}
