//! Связка `AppState` с действиями субтитров из UI: кнопка CC, меню, клавиши C/V/Z/X.
//!
//! # Границы
//! - Состояние (включено ли, дорожка, сдвиг) и решение «что сообщить» принадлежат владельцу
//!   субтитров (`SubtitleOwner::apply_control_action`, `state/subtitles/control_actions.rs`).
//! - Здесь только то, что требует `AppState`: показать уведомление и открыть системный диалог
//!   «Load file…» (нужно окно). Диалог принадлежит `SubtitleFilePicker`; ответ приходит по
//!   wake-событию субтитров и обрабатывается в [`AppState::poll_subtitle_file_picker`].

use std::time::Instant;

use tracing::warn;
use winit::window::Window;

use super::AppState;
use super::notifications::NotificationCenter;
use super::subtitles::ControlActionFeedback;
use crate::subtitle_file_open::{PickerStart, SubtitleFilePickResult};
use crate::subtitle_message::attach_outcome_message;
use crate::ui::player_controls::SubtitleControlAction;

/// Сообщение, если системный диалог выбора файла не удалось открыть.
const FILE_DIALOG_UNAVAILABLE_MESSAGE: &str = "Could not open the file dialog.";

impl AppState {
    /// Применяет одно действие субтитров из UI после egui pass.
    pub(super) fn apply_subtitle_control_action(
        &mut self,
        action: SubtitleControlAction,
        window: &Window,
    ) {
        if action == SubtitleControlAction::OpenLoadFileDialog {
            self.open_subtitle_file_dialog(window);
            return;
        }
        let feedback = self.subtitles.apply_control_action(action);
        deliver_feedback(&mut self.notifications, feedback, Instant::now());
        self.mark_pending_worker_redraw();
    }

    /// Открывает диалог «Load file…» (защита от второго диалога — у владельца диалога).
    fn open_subtitle_file_dialog(&mut self, window: &Window) {
        match self.subtitle_file_picker.open_dialog(window) {
            PickerStart::Started | PickerStart::AlreadyOpen => {}
            PickerStart::SpawnFailed { reason } => {
                warn!(%reason, "Could not start the subtitle file dialog");
                self.notify_transient(FILE_DIALOG_UNAVAILABLE_MESSAGE);
            }
        }
    }

    /// Забирает ответ диалога «Load file…» и добавляет выбранный файл к media.
    ///
    /// Возвращает `true`, если что-то изменилось и нужна перерисовка.
    pub(super) fn poll_subtitle_file_picker(&mut self) -> bool {
        match self.subtitle_file_picker.poll() {
            Some(SubtitleFilePickResult::Picked(path)) => {
                let outcome = self.attach_subtitle_file(&path);
                if let Some(message) = attach_outcome_message(outcome) {
                    self.notify_transient(message);
                }
                true
            }
            Some(SubtitleFilePickResult::Cancelled | SubtitleFilePickResult::DialogLost) | None => {
                false
            }
        }
    }
}

/// Показывает обратную связь владельца субтитров в центре уведомлений.
fn deliver_feedback(
    notifications: &mut NotificationCenter,
    feedback: ControlActionFeedback,
    now: Instant,
) {
    match feedback {
        ControlActionFeedback::Silent => {}
        ControlActionFeedback::Toast(text) => {
            notifications.notify_transient(text, now);
        }
        ControlActionFeedback::ReplacingToast { key, text } => {
            notifications.notify_transient_replacing(key, text, now);
        }
    }
}

#[cfg(test)]
mod tests;
