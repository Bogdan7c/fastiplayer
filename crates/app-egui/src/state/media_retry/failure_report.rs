//! Владельцы ошибок открытия: текст, причина и цель повтора собираются в одном месте.
//!
//! Здесь решается, *что* повторять для ошибки каждого происхождения (файл из Open, элемент
//! очереди, ссылка, оборванное воспроизведение), а осмысленность повтора по причине
//! отдаёт `MediaOpenUserFailureReason::retry_advice`.

use std::path::Path;

use tracing::debug;

use super::{MediaRetryTarget, RetryPlan};
use crate::local_open_message::{failure_reason_phrase, local_open_failure_message};
use crate::media_open::MediaOpenUserFailureReason;
use crate::playlist_runtime::PlaylistRuntime;
use crate::state::{AppState, StrongMediaOpenError, StrongMediaOpenUserOutcome};
use crate::web_open_message::web_open_failure_message;

/// Начало текста для элемента очереди, имя которого в центре не показывается.
const QUEUE_ITEM_FAILURE_PREFIX: &str = "Не удалось открыть элемент очереди";

impl AppState {
    /// Ошибка открытия локального файла (кнопка Open, drag & drop, повтор): «Не удалось
    /// открыть «clip.mkv»: файл не найден» + «Повторить», если причина внешняя.
    pub(crate) fn report_local_open_failure(
        &mut self,
        path: &Path,
        reason: impl Into<MediaOpenUserFailureReason>,
    ) {
        let reason = reason.into();
        let message = local_open_failure_message(path, reason);
        let retry = RetryPlan::for_reason(reason, MediaRetryTarget::LocalFile(path.to_path_buf()));
        self.report_open_failure(message, retry);
    }

    /// Ручной выбор элемента очереди не открылся: ошибка в центре (кроме строки очереди).
    ///
    /// Очередь сама решает, что отказ был *ручным* (`take_manual_open_failure`): шаги
    /// автоматического пропуска сюда не попадают, их итог показывает цепочка пропусков.
    /// Отмена и «занято» ошибкой не считаются (как и для локального файла).
    pub(crate) fn show_manual_open_failure(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        error: &StrongMediaOpenError,
    ) {
        let Some(item_id) = playlist_runtime.take_manual_open_failure() else {
            return;
        };
        let StrongMediaOpenUserOutcome::Failed(reason) = error.user_outcome() else {
            debug!(
                ?item_id,
                "Ручное открытие не удалось без причины для показа"
            );
            return;
        };
        let message = match reason {
            MediaOpenUserFailureReason::WebOpen(web_reason) => {
                web_open_failure_message(None, web_reason)
            }
            MediaOpenUserFailureReason::Preparation(_)
            | MediaOpenUserFailureReason::PlayerInstall(_) => {
                format!(
                    "{QUEUE_ITEM_FAILURE_PREFIX}: {}",
                    failure_reason_phrase(reason)
                )
            }
        };
        self.report_open_failure(
            message,
            RetryPlan::for_reason(reason, MediaRetryTarget::QueueItem(item_id)),
        );
    }
}
