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
//!
//! У web-источника с несколькими версиями звука строки — это версии («English (E-AC-3 5.1)»);
//! клик по ним публикует [`AudioTrackControlAction::SwitchWebAudioVersion`].

use egui::{RadioButton, Ui};

use super::{AudioTrackControlAction, ControlAction};
use crate::state::{AudioTrackMenuTarget, AudioTrackMenuView};

/// Минимальная ширина меню: длинные подписи не должны прыгать от открытия к открытию.
const MENU_MIN_WIDTH: f32 = 240.0;

/// Рисует пункты меню и добавляет действие выбранной дорожки.
pub(super) fn show(ui: &mut Ui, view: &AudioTrackMenuView, actions: &mut Vec<ControlAction>) {
    ui.set_min_width(MENU_MIN_WIDTH);
    for track in view.tracks() {
        let item = ui.add(RadioButton::new(track.is_selected, track.label.as_str()));
        if item.clicked() {
            actions.push(ControlAction::AudioTracks(control_action_for(track.target)));
            ui.close();
        }
    }
}

/// Действие для строки меню: встроенная дорожка или web-версия звука.
fn control_action_for(target: AudioTrackMenuTarget) -> AudioTrackControlAction {
    match target {
        AudioTrackMenuTarget::EmbeddedTrack(track_id) => {
            AudioTrackControlAction::SelectTrack(track_id)
        }
        AudioTrackMenuTarget::WebAudioVersion(version) => {
            AudioTrackControlAction::SwitchWebAudioVersion(version)
        }
    }
}
