//! Применение действий UI (кнопка CC, меню, клавиши C/V/Z/X) к владельцу субтитров.
//!
//! Метод [`SubtitleOwner::apply_control_action`] делает всё, что относится к состоянию
//! субтитров, и возвращает [`ControlActionFeedback`]: что стоит сообщить пользователю.
//! Показ уведомления остаётся за `AppState` (центр уведомлений и окно принадлежат ему),
//! поэтому логику можно проверять настоящим владельцем без окна.
//!
//! # Правило «нет дорожек»
//! Если у видео нет ни одной дорожки, действия C/V/Z/X ничего не меняют и отвечают
//! «No subtitles for this video»: пользователь нажал клавишу и должен понять, почему ничего
//! не произошло. Предпочтение «субтитры включены» при этом не трогается.

use subtitle_core::SubtitleTrackId;

use super::SubtitleOwner;
use super::intents::{CycleOutcome, SelectTrackOutcome};
use super::menu_view::SubtitleMenuView;
use crate::state::notifications::TransientReplaceKey;
use crate::subtitle_message::{
    NO_SUBTITLES_MESSAGE, SUBTITLES_OFF_MESSAGE, TrackNumber, sync_offset_toast,
    track_selected_toast, track_text,
};
use crate::ui::player_controls::SubtitleControlAction;

/// Что сообщить пользователю после действия.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControlActionFeedback {
    /// Результат уже виден сам (подчёркивание CC, сами субтитры): уведомление не нужно.
    Silent,
    /// Отдельное временное уведомление.
    Toast(String),
    /// Уведомление, заменяющее прежнее с тем же ключом (серия нажатий Z/X или V).
    ReplacingToast {
        key: TransientReplaceKey,
        text: String,
    },
}

impl SubtitleOwner {
    /// Применяет действие UI и возвращает обратную связь для пользователя.
    ///
    /// `OpenLoadFileDialog` состояния субтитров не меняет: диалог открывает вызывающий,
    /// у которого есть окно, поэтому здесь ответ `Silent`.
    pub(crate) fn apply_control_action(
        &mut self,
        action: SubtitleControlAction,
    ) -> ControlActionFeedback {
        match action {
            SubtitleControlAction::OpenLoadFileDialog => ControlActionFeedback::Silent,
            SubtitleControlAction::Disable => {
                self.set_visibility(super::intents::SubtitleVisibility::Hidden);
                ControlActionFeedback::Silent
            }
            SubtitleControlAction::SelectTrack(track_id) => self.select_from_menu(track_id),
            // Остальные действия имеют смысл только при наличии дорожек.
            SubtitleControlAction::ToggleVisibility
            | SubtitleControlAction::SelectNextTrack
            | SubtitleControlAction::NudgeSync(_)
            | SubtitleControlAction::ResetSync
                if !self.menu_view().has_tracks =>
            {
                ControlActionFeedback::Toast(NO_SUBTITLES_MESSAGE.to_owned())
            }
            SubtitleControlAction::ToggleVisibility => {
                let _ = self.toggle_visibility();
                ControlActionFeedback::Silent
            }
            SubtitleControlAction::SelectNextTrack => self.cycle_track(),
            SubtitleControlAction::NudgeSync(nudge) => {
                let offset = self.nudge_sync(nudge);
                ControlActionFeedback::ReplacingToast {
                    key: TransientReplaceKey::SubtitleSync,
                    text: sync_offset_toast(offset),
                }
            }
            SubtitleControlAction::ResetSync => {
                self.reset_sync();
                ControlActionFeedback::ReplacingToast {
                    key: TransientReplaceKey::SubtitleSync,
                    text: sync_offset_toast(self.menu_view().sync_offset),
                }
            }
        }
    }

    /// Клик по дорожке в меню: выбор, а при отказе — понятное сообщение.
    fn select_from_menu(&mut self, track_id: SubtitleTrackId) -> ControlActionFeedback {
        match self.select_track(track_id) {
            SelectTrackOutcome::Selected => ControlActionFeedback::Silent,
            SelectTrackOutcome::TrackUnavailable => {
                ControlActionFeedback::Toast("This subtitle file cannot be used.".to_owned())
            }
            SelectTrackOutcome::UnknownTrack => {
                // Дорожка пропала между кадром меню и применением клика: сообщать нечего.
                tracing::debug!("Subtitle track disappeared before the menu click was applied");
                ControlActionFeedback::Silent
            }
        }
    }

    /// Клавиша V: следующая дорожка, затем «выкл», затем снова первая.
    fn cycle_track(&mut self) -> ControlActionFeedback {
        let text = match self.select_next_track() {
            CycleOutcome::Track(track_id) => match menu_track_text(&self.menu_view(), track_id) {
                Some(text) => track_selected_toast(&text),
                None => return ControlActionFeedback::Silent,
            },
            CycleOutcome::Off => SUBTITLES_OFF_MESSAGE.to_owned(),
            // Дорожки есть, но выбрать нечего (все недоступны).
            CycleOutcome::NoTracks => {
                return ControlActionFeedback::Toast(NO_SUBTITLES_MESSAGE.to_owned());
            }
        };
        ControlActionFeedback::ReplacingToast {
            key: TransientReplaceKey::SubtitleTrack,
            text,
        }
    }
}

/// Подпись дорожки с этим id в меню (с учётом её позиции для «Track N»).
fn menu_track_text(view: &SubtitleMenuView, track_id: SubtitleTrackId) -> Option<String> {
    let (menu_index, track) = view
        .tracks
        .iter()
        .enumerate()
        .find(|(_, track)| track.id == track_id)?;
    Some(track_text(
        &track.label,
        TrackNumber::from_menu_index(menu_index),
    ))
}
