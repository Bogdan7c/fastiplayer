//! Содержимое меню аудиодорожек: радио-список с отмеченной текущей дорожкой.
//!
//! Меню только показывает снимок [`AudioTrackMenuView`] и публикует типизированные действия.
//! Клик по дорожке закрывает меню (`ui.close()`).
//!
//! ```text
//! ( ) English — Original (Opus)
//! (•) Russian — Dub (FLAC)
//! ( ) Track 3 (AAC)
//! ```

use egui::{RadioButton, Ui};

use super::{AudioTrackControlAction, ControlAction};
use crate::state::AudioTrackMenuView;

/// Минимальная ширина меню: длинные подписи не должны прыгать от открытия к открытию.
const MENU_MIN_WIDTH: f32 = 240.0;

/// Рисует пункты меню и добавляет действие выбранной дорожки.
pub(super) fn show(ui: &mut Ui, view: &AudioTrackMenuView, actions: &mut Vec<ControlAction>) {
    ui.set_min_width(MENU_MIN_WIDTH);
    for track in view.tracks() {
        let item = ui.add(RadioButton::new(track.is_selected, track.label.as_str()));
        if item.clicked() {
            actions.push(ControlAction::AudioTracks(
                AudioTrackControlAction::SelectTrack(track.id),
            ));
            ui.close();
        }
    }
}
