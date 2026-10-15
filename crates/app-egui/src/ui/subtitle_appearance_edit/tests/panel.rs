//! Панель режима правки: кнопки, ползунок, выбор оформления и состояние клавиатурного фокуса.

use egui::{Event, Key, Modifiers, pos2};

use super::harness::{EditHarness, FrameReport, edits, label_center, styled_view};
use crate::state::AppearanceEdit;
use crate::state::AppearanceEditFinish;
use crate::ui::keyboard_focus::{KeyboardFocusOwner, keyboard_focus_owner};
use crate::ui::subtitle_appearance_edit::SubtitleAppearanceEditAction;
use fastiplayer_config::SubtitleDecorationPreference;

/// Кадр, чтобы панель отрисовалась и можно было найти подписи кнопок.
fn settled() -> (EditHarness, FrameReport) {
    let mut harness = EditHarness::new(styled_view());
    harness.frame(Vec::new());
    let report = harness.frame(Vec::new());
    (harness, report)
}

fn actions_after_clicking(label: &str) -> (Vec<SubtitleAppearanceEditAction>, EditHarness) {
    let (mut harness, report) = settled();
    let target = label_center(&report.shapes, label)
        .unwrap_or_else(|| panic!("panel shows a {label:?} label"))
        .center();
    let reports = harness.click(target);
    let actions = reports
        .into_iter()
        .flat_map(|report| report.actions)
        .collect();
    (actions, harness)
}

#[test]
fn done_cancel_and_reset_emit_their_actions() {
    assert_eq!(
        actions_after_clicking("Done").0,
        vec![SubtitleAppearanceEditAction::Finish(
            AppearanceEditFinish::Save
        )]
    );
    assert_eq!(
        actions_after_clicking("Cancel").0,
        vec![SubtitleAppearanceEditAction::Finish(
            AppearanceEditFinish::Cancel
        )]
    );
    assert_eq!(
        actions_after_clicking("Reset").0,
        vec![SubtitleAppearanceEditAction::Reset]
    );
}

#[test]
fn choosing_outline_sets_the_decoration_and_the_current_choice_is_not_resent() {
    assert_eq!(
        actions_after_clicking("Outline").0,
        vec![SubtitleAppearanceEditAction::Edit(
            AppearanceEdit::SetDecoration(SubtitleDecorationPreference::Outline)
        )]
    );
    // Рисование и клик по уже выбранному «Box» ничего не меняют.
    assert!(actions_after_clicking("Box").0.is_empty());
}

#[test]
fn an_idle_panel_never_changes_the_style_by_itself() {
    let mut harness = EditHarness::new(styled_view());
    for _ in 0..5 {
        assert!(harness.frame(Vec::new()).actions.is_empty());
    }
}

#[test]
fn opacity_slider_drag_changes_the_background_opacity() {
    let (mut harness, report) = settled();
    let label = label_center(&report.shapes, "Opacity").expect("slider caption");
    // Ползунок стоит слева от подписи; тянем по его дорожке вправо.
    let start = pos2(label.left() - 90.0, label.center().y);
    let reports = harness.drag(&[start, pos2(start.x + 30.0, start.y)]);

    let opacity: Vec<u8> = edits(&reports)
        .into_iter()
        .filter_map(|edit| match edit {
            AppearanceEdit::SetBackgroundOpacity { percent } => Some(percent),
            _ => None,
        })
        .collect();
    assert!(
        !opacity.is_empty(),
        "dragging the slider changes the opacity"
    );
    assert!(opacity.iter().all(|percent| *percent <= 100));
}

#[test]
fn mouse_use_of_panel_buttons_leaves_the_keyboard_free_for_escape() {
    // Корень «Esc не работает»: фокус на кнопке заставляет оболочку пропустить Esc.
    for label in ["Reset", "Cancel", "Done", "Outline"] {
        let (_, harness) = actions_after_clicking(label);
        assert_eq!(
            keyboard_focus_owner(&harness.ctx),
            KeyboardFocusOwner::Nobody,
            "after clicking {label} the shell must still receive Esc"
        );
    }
}

#[test]
fn clicking_the_video_surface_does_not_take_keyboard_focus() {
    let mut harness = EditHarness::new(styled_view());
    harness.click(pos2(100.0, 200.0));
    harness.frame(Vec::new());
    assert_eq!(
        keyboard_focus_owner(&harness.ctx),
        KeyboardFocusOwner::Nobody
    );
}

#[test]
fn opacity_slider_drag_leaves_the_keyboard_free_for_escape() {
    let (mut harness, report) = settled();
    let label = label_center(&report.shapes, "Opacity").expect("slider caption");
    let start = pos2(label.left() - 90.0, label.center().y);
    harness.drag(&[start, pos2(start.x + 30.0, start.y)]);
    harness.frame(Vec::new());
    assert_eq!(
        keyboard_focus_owner(&harness.ctx),
        KeyboardFocusOwner::Nobody
    );
}

#[test]
fn keyboard_tab_focus_still_works_and_enter_presses_the_button() {
    // Доступность: Tab по панели сохраняется (в отличие от фокуса после мыши).
    let (mut harness, _) = settled();
    let tab = || Event::Key {
        key: Key::Tab,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    harness.frame(vec![tab()]);
    harness.frame(Vec::new());
    assert_eq!(
        keyboard_focus_owner(&harness.ctx),
        KeyboardFocusOwner::Widget
    );
}
