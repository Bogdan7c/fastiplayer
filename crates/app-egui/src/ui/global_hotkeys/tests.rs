//! Глобальные клавиши: кто и когда получает `+`/`-`/`0`/`↑`/`↓`/`Ctrl+O`.

use egui::{Event, Key, Modifiers};

use super::*;
use crate::ui::test_frame::{input_at, run_ui_frame};

fn key_press(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// Кто держит фокус в тесте.
#[derive(Clone, Copy)]
enum FocusHolder {
    Nobody,
    TextField,
    Button,
}

/// Прогоняет два кадра: в первом фокус получает `holder`, во втором приходят `events`.
/// Возвращает действия второго кадра и признак «стрелка вниз осталась неизрасходованной».
fn actions_with_focus(holder: FocusHolder, events: Vec<Event>) -> (Vec<ControlAction>, bool) {
    let egui_ctx = crate::ui::test_frame::app_behavior_context();
    let mut text = String::new();
    let mut draw_focus_target = |ui: &mut egui::Ui, request_focus: bool| match holder {
        FocusHolder::Nobody => {}
        FocusHolder::TextField => {
            let response = ui.text_edit_singleline(&mut text);
            if request_focus {
                response.request_focus();
            }
        }
        FocusHolder::Button => {
            let response = ui.button("Button");
            if request_focus {
                response.request_focus();
            }
        }
    };
    run_ui_frame(&egui_ctx, input_at(0.0, Vec::new()), |ui| {
        draw_focus_target(ui, true);
    });
    let mut actions = Vec::new();
    let mut arrow_left_for_widget = false;
    run_ui_frame(&egui_ctx, input_at(0.1, events), |ui| {
        draw_focus_target(ui, false);
        actions = collect_actions(ui.ctx());
        arrow_left_for_widget = ui.input(|input| input.key_pressed(Key::ArrowDown));
    });
    (actions, arrow_left_for_widget)
}

fn actions(holder: FocusHolder, events: Vec<Event>) -> Vec<ControlAction> {
    actions_with_focus(holder, events).0
}

#[test]
fn speed_keys_work_globally_without_any_focus() {
    assert_eq!(
        actions(
            FocusHolder::Nobody,
            vec![key_press(Key::Equals, Modifiers::SHIFT)]
        ),
        vec![ControlAction::AdjustPlaybackRateSteps(1)]
    );
    assert_eq!(
        actions(
            FocusHolder::Nobody,
            vec![key_press(Key::Minus, Modifiers::NONE)]
        ),
        vec![ControlAction::AdjustPlaybackRateSteps(-1)]
    );
    assert_eq!(
        actions(
            FocusHolder::Nobody,
            vec![key_press(Key::Num0, Modifiers::NONE)]
        ),
        vec![ControlAction::ResetPlaybackRate]
    );
}

#[test]
fn plus_and_minus_in_one_frame_cancel_and_repeats_accumulate() {
    assert!(
        actions(
            FocusHolder::Nobody,
            vec![
                key_press(Key::Plus, Modifiers::NONE),
                key_press(Key::Minus, Modifiers::NONE)
            ]
        )
        .is_empty()
    );
    assert_eq!(
        actions(
            FocusHolder::Nobody,
            vec![
                key_press(Key::Plus, Modifiers::NONE),
                key_press(Key::Equals, Modifiers::NONE)
            ]
        ),
        vec![ControlAction::AdjustPlaybackRateSteps(2)]
    );
}

#[test]
fn speed_keys_still_work_when_a_button_has_focus() {
    assert_eq!(
        actions(
            FocusHolder::Button,
            vec![key_press(Key::Minus, Modifiers::NONE)]
        ),
        vec![ControlAction::AdjustPlaybackRateSteps(-1)]
    );
}

#[test]
fn arrows_change_volume_only_when_nobody_owns_keyboard() {
    assert_eq!(
        actions(
            FocusHolder::Nobody,
            vec![key_press(Key::ArrowUp, Modifiers::NONE)]
        ),
        vec![ControlAction::AdjustVolumeSteps(1)]
    );
    assert_eq!(
        actions(
            FocusHolder::Nobody,
            vec![
                key_press(Key::ArrowDown, Modifiers::NONE),
                key_press(Key::ArrowDown, Modifiers::NONE)
            ]
        ),
        vec![ControlAction::AdjustVolumeSteps(-2)]
    );
}

#[test]
fn focused_widget_keeps_arrows_for_itself() {
    let (actions, arrow_still_available_to_widget) = actions_with_focus(
        FocusHolder::Button,
        vec![key_press(Key::ArrowDown, Modifiers::NONE)],
    );

    assert!(
        actions.is_empty(),
        "Volume should not be changed: {actions:?}"
    );
    assert!(
        arrow_still_available_to_widget,
        "The arrow must remain the widget (playlist navigation)"
    );
}

#[test]
fn text_input_blocks_every_global_hotkey() {
    let events = vec![
        key_press(Key::Minus, Modifiers::NONE),
        key_press(Key::Plus, Modifiers::NONE),
        key_press(Key::Equals, Modifiers::SHIFT),
        key_press(Key::Num0, Modifiers::NONE),
        key_press(Key::ArrowUp, Modifiers::NONE),
        key_press(Key::ArrowDown, Modifiers::NONE),
        key_press(Key::O, Modifiers::COMMAND),
    ];

    assert!(actions(FocusHolder::TextField, events).is_empty());
}

#[test]
fn ctrl_o_requests_the_same_intent_as_the_open_button() {
    assert_eq!(
        actions(
            FocusHolder::Nobody,
            vec![key_press(Key::O, Modifiers::COMMAND)]
        ),
        vec![ControlAction::OpenFile]
    );
}

#[test]
fn plain_o_and_ctrl_arrows_do_not_trigger_anything() {
    assert!(
        actions(
            FocusHolder::Nobody,
            vec![
                key_press(Key::O, Modifiers::NONE),
                key_press(Key::ArrowUp, Modifiers::COMMAND),
            ]
        )
        .is_empty()
    );
}

/// Действие субтитров в обёртке `ControlAction`.
fn subtitles(action: SubtitleControlAction) -> ControlAction {
    ControlAction::Subtitles(action)
}

#[test]
fn subtitle_keys_map_to_their_actions_without_any_focus() {
    for (key, expected) in [
        (Key::C, SubtitleControlAction::ToggleVisibility),
        (Key::V, SubtitleControlAction::SelectNextTrack),
        (Key::Z, SubtitleControlAction::NudgeSync(SyncNudge::Earlier)),
        (Key::X, SubtitleControlAction::NudgeSync(SyncNudge::Later)),
    ] {
        assert_eq!(
            actions(FocusHolder::Nobody, vec![key_press(key, Modifiers::NONE)]),
            vec![subtitles(expected)],
            "{key:?}"
        );
    }
}

#[test]
fn subtitle_keys_work_while_a_button_has_focus_but_not_while_typing() {
    assert_eq!(
        actions(
            FocusHolder::Button,
            vec![key_press(Key::C, Modifiers::NONE)]
        ),
        vec![subtitles(SubtitleControlAction::ToggleVisibility)],
        "buttons do not use letters"
    );
    for key in [Key::C, Key::V, Key::Z, Key::X] {
        assert!(
            actions(
                FocusHolder::TextField,
                vec![key_press(key, Modifiers::NONE)]
            )
            .is_empty(),
            "{key:?} is text input while a text field has focus"
        );
    }
}

#[test]
fn subtitle_keys_with_ctrl_are_left_to_the_system_shortcuts() {
    for key in [Key::C, Key::V, Key::Z, Key::X] {
        assert!(
            actions(
                FocusHolder::Nobody,
                vec![key_press(key, Modifiers::COMMAND)]
            )
            .is_empty(),
            "Ctrl+{key:?} (copy/paste/undo/cut) must not touch subtitles"
        );
    }
}

#[test]
fn repeated_nudge_presses_in_one_frame_are_separate_steps() {
    assert_eq!(
        actions(
            FocusHolder::Nobody,
            vec![
                key_press(Key::X, Modifiers::NONE),
                key_press(Key::X, Modifiers::NONE),
                key_press(Key::Z, Modifiers::NONE),
            ]
        ),
        vec![
            subtitles(SubtitleControlAction::NudgeSync(SyncNudge::Earlier)),
            subtitles(SubtitleControlAction::NudgeSync(SyncNudge::Later)),
            subtitles(SubtitleControlAction::NudgeSync(SyncNudge::Later)),
        ],
        "each press counts once; order follows the key table (C, V, Z, X)"
    );
}
