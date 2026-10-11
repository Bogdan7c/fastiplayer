//! Отмена подготовки локального файла, которую ведёт `AppState` (кнопка Open, drag & drop,
//! forwarding второго экземпляра, «Повторить» для локального файла).
//!
//! Владелец подготовки — `AppState::local_file_open_job`. Токен отмены уже есть у самого
//! job-а (`LocalFileOpenJob::request_cancellation`); здесь только intent-граница для
//! транспортной команды Stop. Параллельного механизма нет: после отмены worker публикует
//! штатный `LocalFileOpenResult::Cancelled`, который `apply_local_file_open_result`
//! уже обрабатывает молча (без ошибки, без уведомления, без установки).
//!
//! Диалог выбора файла не отменяется: он принадлежит пользователю, а не транспорту.
//! Установка (`LocalOpenInstallOwner`, coordinator-request уже есть) отменяется прежним
//! guard-ом транспорта, поэтому сюда не входит.

use super::AppState;
use crate::media_open::{PreparationCancelCause, PreparationCancelOutcome};
use crate::startup_readiness::StartupReadinessAbortReason;

impl AppState {
    /// Отменяет идущую подготовку локального файла, если она есть.
    ///
    /// Не ждёт выхода worker-а (join остаётся за обычным `drain`), поэтому безопасен на
    /// UI-потоке. Строка «Открываем «…»…» снимается сразу, чтобы Stop был виден мгновенно;
    /// слот job-а освободится, когда worker отдаст `Cancelled`.
    pub(crate) fn cancel_pending_local_file_preparation(
        &mut self,
        cause: PreparationCancelCause,
    ) -> PreparationCancelOutcome {
        let Some(job) = self.local_file_open_job.as_ref() else {
            return PreparationCancelOutcome::NothingPending;
        };
        // Решение «диалог не трогаем» принимает сам job: он знает свою фазу.
        let outcome = job.cancel_preparation(cause);
        if outcome.cancelled_anything() {
            self.startup_pending = None;
            self.mark_pending_worker_redraw();
            tracing::info!("Подготовка локального файла отменена по Stop");
        }
        outcome
    }

    /// Снимает строку «Подготовка…» и закрывает измерение startup после отмены владельцем
    /// стартовой подготовки (`StartupMediaController`).
    ///
    /// Не показывает ошибку и не трогает уведомления: отмена по Stop для пользователя тихая.
    pub(crate) fn withdraw_cancelled_startup_preparation(&mut self) {
        self.startup_pending = None;
        // Пользовательский Stop вытесняет стартовую попытку (отдельной причины нет: файл
        // телеметрии у лимита размера).
        self.abort_startup_readiness(StartupReadinessAbortReason::Superseded);
        self.mark_pending_worker_redraw();
    }
}
