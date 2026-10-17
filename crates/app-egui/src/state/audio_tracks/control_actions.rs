//! Применение действий выбора аудио (меню «♪», клавиша `B`) и воронка команды плееру.
//!
//! # Границы
//! - Что выбирать (следующая дорожка, подписи) решает [`AudioTrackMenuView`] — чистая функция
//!   snapshot-а.
//! - Как отправить команду и что сказать пользователю при отказе — здесь, в одной воронке
//!   [`AppState::select_audio_track_by_user`]. Воронка не владеет списком дорожек и текущим
//!   выбором: они принадлежат player-у и приходят в аргументах.
//! - Отправка вынесена за порт [`PlayerCommandPort`], чтобы поведение воронки проверялось
//!   без живого воркера (переполненная очередь, обрыв канала).
//!
//! # Точка расширения
//! Память выбора «для этого видео» подключается в `select_audio_track_by_user` после
//! успешной отправки ([`AudioTrackSelectionFeedback::Requested`]). Все пользовательские
//! выборы аудио обязаны идти через неё.

use std::time::Instant;

use media_core::TrackId;
use player_core::{PlayerCommand, PlayerSnapshot, PlayerWorker, PlayerWorkerSendError};
use tracing::warn;

use super::labels::audio_selected_toast;
use super::menu_view::{AudioTrackMenuTarget, AudioTrackMenuView};
use crate::state::AppState;
use crate::state::notifications::TransientReplaceKey;
use crate::ui::player_controls::AudioTrackControlAction;

/// Toast, если команда выбора дорожки не дошла до плеера.
pub(super) const SWITCH_FAILED_MESSAGE: &str = "Could not switch audio track";
/// Toast, если нажали `B`, а аудиодорожка в media одна.
pub(super) const SINGLE_TRACK_MESSAGE: &str = "This video has only one audio track";
/// Toast, если нажали `B`, а аудиодорожек в media нет (или media не открыто).
pub(super) const NO_AUDIO_MESSAGE: &str = "This video has no audio tracks";

/// Итог пользовательского выбора аудиодорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioTrackSelectionFeedback {
    /// Команда принята очередью плеера; фактическое переключение придёт в snapshot.
    Requested,
    /// Эта дорожка уже выбрана: команда не отправлялась.
    AlreadySelected,
    /// Команда не отправлена (очередь переполнена или воркер остановлен); пользователь уведомлён.
    SendFailed(PlayerWorkerSendError),
}

/// Куда воронка отправляет команды плееру (в проде — воркер, в тестах — заглушка).
pub(super) trait PlayerCommandPort {
    /// Неблокирующая отправка; ошибка различает «очередь полна» и «воркер остановлен».
    fn try_send(&self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError>;
}

impl PlayerCommandPort for PlayerWorker {
    fn try_send(&self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        self.try_send_command(command)
    }
}

/// Чистая часть воронки: пропускает уже выбранную дорожку и отправляет команду.
pub(super) fn send_audio_selection(
    port: &impl PlayerCommandPort,
    currently_selected: Option<TrackId>,
    track_id: TrackId,
) -> AudioTrackSelectionFeedback {
    if currently_selected == Some(track_id) {
        return AudioTrackSelectionFeedback::AlreadySelected;
    }
    match port.try_send(PlayerCommand::SelectAudioTrack(track_id)) {
        Ok(()) => AudioTrackSelectionFeedback::Requested,
        Err(error) => AudioTrackSelectionFeedback::SendFailed(error),
    }
}

/// Что сделать по клавише `B` (чистое решение по снимку меню).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NextTrackPlan {
    /// Аудио нет вовсе.
    NoAudio,
    /// Дорожка одна: переключать некуда.
    SingleTrack,
    /// Переключиться на строку `target`; `label` — её подпись для toast-а.
    Switch {
        target: AudioTrackMenuTarget,
        label: String,
    },
}

impl NextTrackPlan {
    /// Текст toast-а, если переключаться некуда; `None` — переключение состоится.
    pub(super) fn notice(&self) -> Option<&'static str> {
        match self {
            Self::NoAudio => Some(NO_AUDIO_MESSAGE),
            Self::SingleTrack => Some(SINGLE_TRACK_MESSAGE),
            Self::Switch { .. } => None,
        }
    }
}

/// Решает, что делает `B` для данного снимка.
pub(super) fn plan_next_track(view: &AudioTrackMenuView) -> NextTrackPlan {
    match view.next_track() {
        Some(next) => NextTrackPlan::Switch {
            target: next.target,
            label: next.label.clone(),
        },
        None if view.track_count() == 0 => NextTrackPlan::NoAudio,
        None => NextTrackPlan::SingleTrack,
    }
}

impl AppState {
    /// Применяет одно действие аудио из UI после egui pass.
    pub(in crate::state) fn apply_audio_track_control_action(
        &mut self,
        action: AudioTrackControlAction,
        player_snapshot: &PlayerSnapshot,
    ) {
        match action {
            AudioTrackControlAction::SelectTrack(track_id) => {
                // Клик по меню: успех виден по самой смене звука, молчим.
                let _ = self.select_audio_track_by_user(player_snapshot, track_id);
            }
            AudioTrackControlAction::SwitchWebAudioVersion(version) => {
                // Переключение версии асинхронное: запрос исполнится после egui pass-а
                // (`apply_requested_web_audio_version`), там же toast о старте/отказе.
                self.request_web_audio_version_by_user(version);
            }
            AudioTrackControlAction::SelectNextTrack => {
                self.select_next_audio_track(player_snapshot)
            }
        }
    }

    /// Клавиша `B`: следующая дорожка по кругу с одной заменяемой плашкой.
    fn select_next_audio_track(&mut self, player_snapshot: &PlayerSnapshot) {
        let view = self.audio_track_menu_view(player_snapshot);
        let plan = plan_next_track(&view);
        if let Some(notice) = plan.notice() {
            self.notify_transient(notice);
            return;
        }
        match plan {
            NextTrackPlan::NoAudio | NextTrackPlan::SingleTrack => {}
            NextTrackPlan::Switch {
                target: AudioTrackMenuTarget::WebAudioVersion(version),
                ..
            } => {
                // Toast с подписью покажет запуск переключения (или отказ) после egui pass-а.
                self.request_web_audio_version_by_user(version);
            }
            NextTrackPlan::Switch {
                target: AudioTrackMenuTarget::EmbeddedTrack(track_id),
                label,
            } => {
                if self.select_audio_track_by_user(player_snapshot, track_id)
                    == AudioTrackSelectionFeedback::Requested
                {
                    self.notifications.notify_transient_replacing(
                        TransientReplaceKey::AudioTrack,
                        audio_selected_toast(&label),
                        Instant::now(),
                    );
                    self.mark_pending_worker_redraw();
                }
            }
        }
    }

    /// ЕДИНАЯ воронка пользовательского выбора аудиодорожки (меню и клавиша `B`).
    ///
    /// Отправляет `SelectAudioTrack(track_id)`; уже выбранную дорожку пропускает без команды.
    /// Отказ очереди не молчаливый: предупреждение в логе и toast пользователю.
    /// После `Requested` выбор запоминается в памяти «для этого видео».
    pub(crate) fn select_audio_track_by_user(
        &mut self,
        player_snapshot: &PlayerSnapshot,
        track_id: TrackId,
    ) -> AudioTrackSelectionFeedback {
        let feedback = send_audio_selection(
            &self.player_worker,
            player_snapshot.selected_tracks.audio_track,
            track_id,
        );
        match feedback {
            AudioTrackSelectionFeedback::Requested => {
                // Пользовательский выбор запоминается «для этого видео» (автоматический - нет).
                self.remember_user_audio_choice(player_snapshot, track_id);
                self.mark_pending_worker_redraw();
            }
            AudioTrackSelectionFeedback::AlreadySelected => {}
            AudioTrackSelectionFeedback::SendFailed(error) => {
                warn!(%error, %track_id, "Could not send the audio track switch to the player");
                self.notify_transient(SWITCH_FAILED_MESSAGE);
            }
        }
        feedback
    }
}
