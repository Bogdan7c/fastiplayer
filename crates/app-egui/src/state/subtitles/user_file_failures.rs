//! Отказы файлов, которые пользователь сам добавил (бросок на окно, «Load file…»).
//!
//! Файл читается в фоне, поэтому исход `AttachOutcome::Attached` приходит раньше результата
//! чтения. Здесь три связанные вещи:
//! 1. классификация типизированной причины в [`SubtitleFileProblem`] (то, что различает человек);
//! 2. [`UserRequestedFiles`] — какие дорожки добавил именно пользователь и что с ними стало.
//!    Автоматически найденные файлы и файлы из памяти выбора сюда не попадают: их отказ виден
//!    только серой подсказкой в меню, иначе каждое видео с битым `.srt` рядом сыпало бы toast-ами;
//! 3. доставка накопленных отказов в центр уведомлений.
//!
//! Тексты принадлежат `subtitle_message`; модуль не знает ни формулировок, ни egui.

use std::collections::HashSet;
use std::time::Instant;

use subtitle_core::SubtitleTrackId;
use subtitle_formats::SubtitleLoadError;

use super::SubtitleOwner;
use super::file_loader::LoadedSidecarTrack;
use super::menu_view::SubtitleFileProblem;
use super::sidecar_job::SubtitleLoadFailure;
use super::track_list::SubtitleTrackList;
use crate::state::AppState;
use crate::state::notifications::NotificationCenter;
use crate::subtitle_message::file_failure_toast;

impl SubtitleLoadFailure {
    /// Причина в терминах, которые различает человек (для toast и серой подсказки меню).
    ///
    /// `match` исчерпывающий: новая причина обязана получить осознанную группу, а не молча
    /// упасть в «прочее».
    pub(super) fn file_problem(&self) -> SubtitleFileProblem {
        match self {
            Self::ReadFailed { .. } => SubtitleFileProblem::CannotOpen,
            Self::FileTooLarge { .. } => SubtitleFileProblem::TooLarge,
            Self::ParseFailed(
                SubtitleLoadError::InputTooLarge { .. }
                | SubtitleLoadError::AssHeaderTooLarge { .. },
            ) => SubtitleFileProblem::TooLarge,
            // Остальные ошибки разбора (лишних реплик, чужой формат, битый XML) и «ни одной
            // реплики» — файл прочитан, но показать из него нечего.
            Self::ParseFailed(_) | Self::NoCuesFound => SubtitleFileProblem::CannotParse,
            Self::SearchFailed(_)
            | Self::FormatNotSupportedYet { .. }
            | Self::EmbeddedCodecNotSupportedYet
            | Self::FontLibraryFailed(_)
            | Self::DownloadFailed(_)
            | Self::WorkerStartFailed { .. }
            | Self::WorkerLost
            | Self::RenderWorkerStartFailed { .. }
            | Self::RenderWorkerLost => SubtitleFileProblem::Other,
        }
    }
}

/// Отказ файла, который пользователь добавил сам: готов к показу toast-ом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UserFileFailure {
    /// Имя файла без каталога (путь каталога пользователю не показываем).
    pub(super) file_name: String,
    /// Группа причины.
    pub(super) problem: SubtitleFileProblem,
}

/// Дорожки media, добавленные пользователем и ещё не получившие результат чтения, плюс
/// накопленные и ещё не показанные отказы. Живёт внутри состояния media и умирает вместе с ним.
#[derive(Debug, Default)]
pub(super) struct UserRequestedFiles {
    /// Дорожки, чей результат ждём ради toast-а.
    awaiting_result: HashSet<SubtitleTrackId>,
    /// Отказы, которые владелец ещё не отдал наружу.
    failures: Vec<UserFileFailure>,
}

impl UserRequestedFiles {
    /// Пользователь добавил дорожку: её отказ заслуживает toast-а.
    pub(super) fn watch(&mut self, track_id: SubtitleTrackId) {
        self.awaiting_result.insert(track_id);
    }

    /// Итог чтения дорожки `track_id`: решает, нужен ли toast. Имя файла берётся из списка
    /// дорожек (только имя, без каталога).
    pub(super) fn record_load_result(
        &mut self,
        tracks: &SubtitleTrackList,
        track_id: SubtitleTrackId,
        result: &Result<LoadedSidecarTrack, SubtitleLoadFailure>,
    ) {
        match result {
            Ok(_) => self.record_success(track_id),
            Err(failure) => {
                let file_name = tracks
                    .get(track_id)
                    .and_then(|entry| entry.description.sidecar_candidate())
                    .and_then(|candidate| candidate.path.file_name())
                    .map(|name| name.to_string_lossy().into_owned());
                self.record_failure(track_id, file_name, failure.file_problem());
            }
        }
    }

    /// Чтение дорожки закончилось отказом. Если дорожку добавил пользователь — отказ запоминается
    /// для toast-а (один раз: наблюдение снимается).
    fn record_failure(
        &mut self,
        track_id: SubtitleTrackId,
        file_name: Option<String>,
        problem: SubtitleFileProblem,
    ) {
        if !self.awaiting_result.remove(&track_id) {
            return;
        }
        self.failures.push(UserFileFailure {
            file_name: file_name.unwrap_or_else(|| "subtitle file".to_owned()),
            problem,
        });
    }

    /// Чтение дорожки закончилось успехом: toast не нужен.
    fn record_success(&mut self, track_id: SubtitleTrackId) {
        self.awaiting_result.remove(&track_id);
    }

    /// Забирает накопленные отказы (повторно не отдаются).
    fn take_failures(&mut self) -> Vec<UserFileFailure> {
        std::mem::take(&mut self.failures)
    }
}

impl SubtitleOwner {
    /// Отказы файлов, добавленных пользователем: возвращаются один раз.
    pub(super) fn take_user_file_failures(&mut self) -> Vec<UserFileFailure> {
        self.media
            .as_mut()
            .map(|media| media.user_files.take_failures())
            .unwrap_or_default()
    }
}

impl AppState {
    /// Показывает toast на каждый отказ файла, который добавил пользователь.
    /// `true` — что-то показано и нужна перерисовка.
    pub(super) fn show_user_file_failures(&mut self) -> bool {
        let failures = self.subtitles.take_user_file_failures();
        deliver_user_file_failures(&mut self.notifications, &failures, Instant::now());
        !failures.is_empty()
    }
}

/// Показывает по toast-у на каждый отказ (отдельная функция: проверяется настоящим центром
/// уведомлений без `AppState`). Подробности причины идут в лог на месте разбора.
pub(super) fn deliver_user_file_failures(
    notifications: &mut NotificationCenter,
    failures: &[UserFileFailure],
    now: Instant,
) {
    for failure in failures {
        notifications
            .notify_transient(file_failure_toast(&failure.file_name, failure.problem), now);
    }
}
