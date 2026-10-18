//! Подменю «Auto-translate» в меню субтитров (web-видео с автопереводами YouTube).
//!
//! Показывает только дорожки-автопереводы из снимка [`SubtitleMenuView`]; порядок строк уже
//! решил владелец субтитров (сверху языки из предпочтений, ниже остальные по алфавиту), UI
//! его не меняет. Список прокручивается: переводов у YouTube бывает больше ста.
//!
//! Подменю не открывается само и не закрывает меню от клика по полосе прокрутки: в egui по
//! умолчанию клик внутри подменю закрывает всё меню, поэтому ему задано
//! `CloseOnClickOutside`, а закрытие делает выбор строки (`ui.close()` поднимается к
//! родительскому меню).

use egui::containers::menu::{MenuConfig, SubMenuButton};
use egui::{PopupCloseBehavior, Ui};

use super::ControlAction;
use super::menu::{TRACK_LIST_MAX_HEIGHT, show_track_row};
use crate::state::SubtitleMenuView;
use crate::ui::egui_behavior::vertical_scroll_area;

/// Заголовок подменю. Стрелку «⏵» справа рисует сам egui.
pub(super) const TRANSLATION_SUBMENU_TITLE: &str = "Auto-translate";

/// Рисует подменю, если у видео есть автопереводы; иначе не рисует ничего.
pub(super) fn show(ui: &mut Ui, view: &SubtitleMenuView, actions: &mut Vec<ControlAction>) {
    if !view
        .tracks
        .iter()
        .any(|track| track.label.is_auto_translation())
    {
        return;
    }
    let config = MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside);
    SubMenuButton::new(TRANSLATION_SUBMENU_TITLE)
        .config(config)
        .ui(ui, |ui| {
            vertical_scroll_area()
                .id_salt("subtitle_translation_tracks")
                .max_height(TRACK_LIST_MAX_HEIGHT)
                .show(ui, |ui| {
                    for (menu_index, track) in view.tracks.iter().enumerate() {
                        if track.label.is_auto_translation() {
                            show_track_row(ui, track, menu_index, actions);
                        }
                    }
                });
        });
}
