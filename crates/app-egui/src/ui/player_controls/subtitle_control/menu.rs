//! Содержимое меню субтитров: дорожки, «Off», «Load file…» и сдвиг синхронизации.
//!
//! Меню только показывает снимок [`SubtitleMenuView`] и публикует типизированные действия;
//! клик по доступному пункту закрывает меню (`ui.close()`), клик по недоступной дорожке —
//! нет (подсказка объясняет причину).
//!
//! ```text
//! (•) Russian (SRT, file)
//! ( ) English (VTT, file)
//! ( ) Track 3 (SRT, file)
//!     Auto-translate ⏵        (только у web-видео с автопереводами, см. `translation_menu`)
//! ───────────
//! ( ) Off
//!     Load file…
//! ───────────
//! Delay: +0.3 s            [Reset]
//! ```

use egui::{Button, RadioButton, Ui};
use subtitle_core::SubtitleSyncOffset;

use super::translation_menu;
use super::{ControlAction, SubtitleControlAction};
use crate::state::{
    SubtitleMenuTrack, SubtitleMenuView, SubtitleTrackAvailability, SubtitleVisibility,
};
use crate::subtitle_message::{
    TrackNumber, sync_offset_menu_text, track_text, unavailable_reason_text,
};
use crate::ui::egui_behavior::vertical_scroll_area;

/// Минимальная ширина меню: длинные подписи дорожек не должны прыгать от открытия к открытию.
const MENU_MIN_WIDTH: f32 = 240.0;

/// Предельная высота списка дорожек (и подменю автоперевода): у видео с десятками языков
/// список прокручивается, а не уходит за экран.
pub(super) const TRACK_LIST_MAX_HEIGHT: f32 = 320.0;

/// Рисует пункты меню и добавляет действия выбранных пунктов.
pub(super) fn show(ui: &mut Ui, view: &SubtitleMenuView, actions: &mut Vec<ControlAction>) {
    ui.set_min_width(MENU_MIN_WIDTH);
    let subtitles_shown = view.visibility == SubtitleVisibility::Shown;

    // Основной список: всё, кроме автопереводов (те — в подменю «Auto-translate»).
    vertical_scroll_area()
        .id_salt("subtitle_main_tracks")
        .max_height(TRACK_LIST_MAX_HEIGHT)
        .show(ui, |ui| {
            for (menu_index, track) in view.tracks.iter().enumerate() {
                if !track.label.is_auto_translation() {
                    show_track_row(ui, track, menu_index, actions);
                }
            }
        });
    translation_menu::show(ui, view, actions);

    ui.separator();
    // «Off» отмечен, когда субтитры скрыты (ни одна дорожка тогда не отмечена).
    if ui.add(RadioButton::new(!subtitles_shown, "Off")).clicked() {
        actions.push(ControlAction::Subtitles(SubtitleControlAction::Disable));
        ui.close();
    }
    if ui.add(Button::new("Load file…")).clicked() {
        actions.push(ControlAction::Subtitles(
            SubtitleControlAction::OpenLoadFileDialog,
        ));
        ui.close();
    }

    ui.separator();
    ui.horizontal(|ui| {
        ui.label(sync_offset_menu_text(view.sync_offset));
        let can_reset = view.sync_offset != SubtitleSyncOffset::ZERO;
        if ui.add_enabled(can_reset, Button::new("Reset")).clicked() {
            actions.push(ControlAction::Subtitles(SubtitleControlAction::ResetSync));
            ui.close();
        }
    });
}

/// Одна строка дорожки: радио-кнопка с подписью. Клик по доступной закрывает всё меню,
/// клик по недоступной — нет (подсказка объясняет причину).
///
/// `menu_index` — позиция в полном списке меню (для «Track N» у дорожек без языка).
pub(super) fn show_track_row(
    ui: &mut Ui,
    track: &SubtitleMenuTrack,
    menu_index: usize,
    actions: &mut Vec<ControlAction>,
) {
    let text = track_text(&track.label, TrackNumber::from_menu_index(menu_index));
    let selectable = matches!(track.availability, SubtitleTrackAvailability::Available);
    let item = ui.add_enabled(selectable, RadioButton::new(track.is_selected, text));
    let item = match &track.availability {
        SubtitleTrackAvailability::Available => item,
        SubtitleTrackAvailability::Unavailable(reason) => {
            item.on_disabled_hover_text(unavailable_reason_text(reason))
        }
    };
    if item.clicked() {
        actions.push(ControlAction::Subtitles(
            SubtitleControlAction::SelectTrack(track.id),
        ));
        ui.close();
    }
}
