//! Режим правки целиком: жест мыши в настоящем кадре egui -> владелец -> сохранённая позиция,
//! а также маршрутизация действий настроек и выход с сохранением.

use egui::{Event, pos2};
use settings_core::SettingValue;

use super::appearance_edit::{INSIDE_CUE_MILLIS, editing_owner, placed_at};
use super::*;
use crate::settings_ui::SettingsUiAction;
use crate::state::subtitle_appearance_edit::{
    SettingsRouting, route_settings_actions, take_style_commit_for_lifecycle_boundary,
};
use crate::state::subtitles::appearance_edit::{AppearanceEdit, AppearanceEditFinish};
use crate::ui::skin::{MinimalSkin, PlayerSkin};
use crate::ui::subtitle_appearance_edit::{
    SubtitleAppearanceEditAction, SubtitleAppearanceEditInput, show,
};
use crate::ui::test_frame::{app_behavior_context, input_at, run_ui_frame};

const VERTICAL_SETTING_ID: &str = "subtitles.position_vertical_percent";

fn committed_integer(
    requests: &[settings_core::RuntimeSettingCommitRequest],
    setting_id: &str,
) -> i64 {
    let request = requests
        .iter()
        .find(|request| request.setting_id.as_str() == setting_id)
        .unwrap_or_else(|| panic!("{setting_id} is part of the style commit"));
    match request.value {
        SettingValue::Integer(value) => value,
        ref other => panic!("{setting_id}: expected an integer, got {other:?}"),
    }
}

/// Кадры egui с событиями; возвращает все действия режима правки.
fn run_frames(
    owner: &mut SubtitleOwner,
    ctx: &egui::Context,
    start_time: f64,
    frames: Vec<Vec<Event>>,
) -> Vec<SubtitleAppearanceEditAction> {
    let style = MinimalSkin.subtitle_appearance_edit_style();
    let mut all_actions = Vec::new();
    for (index, events) in frames.into_iter().enumerate() {
        let view = owner.appearance_edit_view().expect("edit session is open");
        let video_rect = egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        let _ = run_ui_frame(
            ctx,
            input_at(start_time + index as f64 * 0.05, events),
            |ui| {
                all_actions.extend(show(
                    ui,
                    SubtitleAppearanceEditInput {
                        view: &view,
                        video_rect,
                        style: &style,
                    },
                ));
            },
        );
    }
    all_actions
}

#[test]
fn a_mouse_drag_in_a_real_frame_moves_the_stored_position_by_the_dragged_distance() {
    let (mut owner, media, _directory) = editing_owner();
    // Кадр владельца (256x144 px) даёт слой; UI видит его так же, как в приложении.
    placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("the sample or cue is placed");
    let view = owner.appearance_edit_view().expect("session");
    let layer = view.layer_rect.expect("layer");
    let picture = view.picture_rect.expect("picture");
    let start_vertical = view.draft.position.vertical_percent;

    let ctx = app_behavior_context();
    let centre = pos2(
        layer.x as f32 + layer.width as f32 / 2.0,
        layer.y as f32 + layer.height as f32 / 2.0,
    );
    // Прогрев, наведение, нажатие, подъём на 10 pt вверх и отпускание.
    let rise_points = 10.0;
    let moved = pos2(centre.x, centre.y - rise_points);
    let press = |position, pressed| Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let actions = run_frames(
        &mut owner,
        &ctx,
        0.0,
        vec![
            vec![],
            vec![],
            vec![Event::PointerMoved(centre)],
            vec![press(centre, true)],
            vec![Event::PointerMoved(moved)],
            vec![press(moved, false)],
        ],
    );

    for action in actions {
        if let SubtitleAppearanceEditAction::Edit(edit) = action {
            owner.apply_appearance_edit(edit);
        }
    }
    // Масштаб 1.0: 10 pt = 10 px; доля высоты картинки в процентах положения слоя.
    let expected_rise = 10.0 / picture.height * 100.0;
    let session = owner.appearance_edit_view().expect("session");
    let moved_percent = f32::from(session.draft.position.vertical_percent - start_vertical);
    assert!(
        (moved_percent - expected_rise).abs() <= 1.0,
        "moved {moved_percent} %, expected about {expected_rise} %"
    );
    assert!(moved_percent > 0.0, "dragging up raises the subtitles");
}

fn settings_entry() -> Vec<SettingsUiAction> {
    vec![SettingsUiAction::EditSubtitleAppearanceOnVideo]
}

#[test]
fn entry_button_with_a_video_closes_settings_and_opens_the_edit_mode() {
    let (mut owner, _media, _directory) = {
        let media = instance(1);
        let (owner, directory) = ready_owner(media);
        (owner, media, directory)
    };
    let mut actions = settings_entry();

    let routing = route_settings_actions(&mut owner, &mut actions);

    assert_eq!(routing, SettingsRouting::EditStarted);
    assert!(owner.is_editing_appearance());
    assert_eq!(actions, vec![SettingsUiAction::Cancel]);
}

#[test]
fn entry_button_without_a_video_opens_nothing_and_keeps_settings_open() {
    let mut owner = owner();
    let mut actions = settings_entry();

    let routing = route_settings_actions(&mut owner, &mut actions);

    assert_eq!(routing, SettingsRouting::EditNotStarted);
    assert!(!owner.is_editing_appearance());
    assert!(actions.is_empty(), "no Cancel: the sidebar stays open");
}

#[test]
fn reopening_settings_while_editing_saves_the_style() {
    for reopen in [SettingsUiAction::Open, SettingsUiAction::ToggleOpen] {
        let (mut owner, _media, _directory) = editing_owner();
        owner.apply_appearance_edit(AppearanceEdit::MoveBy {
            horizontal_fraction: 0.0,
            vertical_fraction: 0.2,
        });
        let mut actions = vec![reopen.clone()];

        route_settings_actions(&mut owner, &mut actions);

        assert!(!owner.is_editing_appearance(), "{reopen:?}");
        assert_eq!(
            actions,
            vec![reopen.clone()],
            "the settings action is untouched"
        );
        let saved = owner
            .take_style_save_request()
            .expect("style is queued for saving");
        assert_eq!(saved.position.vertical_percent, 26, "6 % default + 20 %");
    }
}

#[test]
fn opening_settings_without_an_edit_session_changes_nothing() {
    let (mut owner, _media, _directory) = {
        let media = instance(1);
        let (owner, directory) = ready_owner(media);
        (owner, media, directory)
    };
    let mut actions = vec![SettingsUiAction::Open];

    assert_eq!(
        route_settings_actions(&mut owner, &mut actions),
        SettingsRouting::EditNotStarted
    );
    assert_eq!(actions, vec![SettingsUiAction::Open]);
    assert_eq!(owner.take_style_save_request(), None);
}

#[test]
fn an_edit_session_at_exit_is_saved_once_with_all_style_values() {
    let (mut owner, _media, _directory) = editing_owner();
    owner.apply_appearance_edit(AppearanceEdit::MoveBy {
        horizontal_fraction: 0.0,
        vertical_fraction: 0.1,
    });

    let requests = take_style_commit_for_lifecycle_boundary(&mut owner)
        .expect("the open session is finished with Save");

    assert!(!owner.is_editing_appearance());
    assert_eq!(committed_integer(&requests, VERTICAL_SETTING_ID), 16);
    assert_eq!(
        requests.len(),
        8,
        "one transaction carries every style setting"
    );
    assert_eq!(
        take_style_commit_for_lifecycle_boundary(&mut owner),
        None,
        "nothing is written twice"
    );
}

#[test]
fn exit_without_edit_session_or_pending_style_writes_nothing() {
    let mut owner = owner();
    assert_eq!(take_style_commit_for_lifecycle_boundary(&mut owner), None);
}

#[test]
fn cancel_before_exit_writes_nothing() {
    let (mut owner, _media, _directory) = editing_owner();
    owner.apply_appearance_edit(AppearanceEdit::MoveBy {
        horizontal_fraction: 0.3,
        vertical_fraction: 0.3,
    });
    owner.finish_appearance_edit(AppearanceEditFinish::Cancel);
    assert_eq!(take_style_commit_for_lifecycle_boundary(&mut owner), None);
}
