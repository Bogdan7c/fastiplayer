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
//! клик по ним публикует [`AudioTrackControlAction::SwitchWebAudioVersion`]. У yt-dlp-видео с
//! дубляжами строки — языки («Russian», «English (original)»), клик публикует
//! [`AudioTrackControlAction::SwitchWebAudioLanguage`].

use egui::{RadioButton, Ui};

use super::{AudioTrackControlAction, ControlAction};
use crate::state::{AudioTrackMenuTarget, AudioTrackMenuView};
use crate::ui::egui_behavior::vertical_scroll_area;

/// Минимальная ширина меню: длинные подписи не должны прыгать от открытия к открытию.
const MENU_MIN_WIDTH: f32 = 240.0;

/// Предельная высота списка дорожек: у MKV с десятками дорожек и yt-dlp-видео с множеством
/// дубляжей список прокручивается, а не уходит за окно. Значение то же, что у меню CC
/// (`subtitle_control::menu::TRACK_LIST_MAX_HEIGHT`), чтобы оба меню вели себя одинаково.
pub(super) const TRACK_LIST_MAX_HEIGHT: f32 = 320.0;

/// Рисует пункты меню и добавляет действие выбранной дорожки.
pub(super) fn show(ui: &mut Ui, view: &AudioTrackMenuView, actions: &mut Vec<ControlAction>) {
    ui.set_min_width(MENU_MIN_WIDTH);
    vertical_scroll_area()
        .id_salt("audio_track_list")
        .max_height(TRACK_LIST_MAX_HEIGHT)
        .show(ui, |ui| {
            for track in view.tracks() {
                let item = ui.add(RadioButton::new(track.is_selected, track.label.as_str()));
                if item.clicked() {
                    actions.push(ControlAction::AudioTracks(control_action_for(track.target)));
                    ui.close();
                }
            }
        });
}

/// Действие для строки меню: встроенная дорожка, web-версия или язык звука.
fn control_action_for(target: AudioTrackMenuTarget) -> AudioTrackControlAction {
    match target {
        AudioTrackMenuTarget::EmbeddedTrack(track_id) => {
            AudioTrackControlAction::SelectTrack(track_id)
        }
        AudioTrackMenuTarget::WebAudioVersion(version) => {
            AudioTrackControlAction::SwitchWebAudioVersion(version)
        }
        AudioTrackMenuTarget::WebAudioLanguage(language) => {
            AudioTrackControlAction::SwitchWebAudioLanguage(language)
        }
    }
}
