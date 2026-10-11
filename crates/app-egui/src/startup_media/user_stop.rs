//! Stop пользователя во время стартовой подготовки источника (UX edge cases 2, D3).
//!
//! Корень проблемы: транспортная команда Stop адресована player-у и controller-у плейлиста,
//! а стартовая подготовка (CLI-ссылка, `yt-dlp`, direct media, нативный HLS/DASH/HDS/Smooth,
//! локальный файл из CLI/restore) живёт в `StartupMediaController` и до появления
//! coordinator-request-а ни на что не подписана. Поэтому Stop ничего не отменял, и через
//! десятки секунд видео всё равно открывалось.
//!
//! Решение: `cancel_pending_preparation(PreparationCancelCause::UserStop)` — intent-метод
//! владельца подготовки. Он:
//! 1. поднимает уже существующие токены отмены у всех живых job-ов (флаг + токен источника:
//!    сеть/yt-dlp прерываются, если соответствующий слой их проверяет);
//! 2. отбрасывает готовый, но ещё не начатый winner (`PreparedAwaitingAllocator`);
//! 3. закрывает orchestration в `Idle` и явно запрещает restored fallback: Stop не должен
//!    превращаться в «открой сохранённый элемент» и не перебирает очередь;
//! 4. запоминает отмену, чтобы итог job-а (даже успешный или ошибочный) был отброшен молча
//!    в [`StartupMediaController::discard_stopped_preparation_jobs`].
//!
//! Job-ы не join-ятся на UI-потоке: слот освобождается, когда worker реально вышел.
//! Установка (coordinator-request уже есть) сюда не входит: её отменяет прежний guard
//! транспорта (C5, тихая отмена `UserCancelled/TransportStop`).

use std::sync::atomic::Ordering;

use super::{StartupMediaController, StartupMediaPhase};
use crate::media_open::{PreparationCancelCause, PreparationCancelOutcome};

/// Забирает итог job-а из слота, если worker уже завершился; сам итог отбрасывается.
///
/// Возвращает `true`, если слот освобождён в этом вызове.
fn discard_if_finished<Job>(
    slot: &mut Option<Job>,
    mut has_finished: impl FnMut(&mut Job) -> bool,
) -> bool {
    let finished = slot.as_mut().is_some_and(&mut has_finished);
    if finished {
        *slot = None;
    }
    finished
}

impl StartupMediaController {
    /// Отменяет стартовую подготовку источника до coordinator-request-а.
    ///
    /// Тихая отмена в семантике C5: ни ошибок, ни уведомлений, цель не помечается битой,
    /// сохранённая очередь не перебирается. Возвращает [`PreparationCancelOutcome::NothingPending`],
    /// если подготовки нет (в том числе когда установка уже идёт — её ведёт guard транспорта).
    pub(crate) fn cancel_pending_preparation(
        &mut self,
        cause: PreparationCancelCause,
    ) -> PreparationCancelOutcome {
        let PreparationCancelCause::UserStop = cause;
        let holds_prepared_winner =
            self.orchestration.prepared.is_some() && self.orchestration.pending_install.is_none();
        if !self.has_active_preparation_job() && !holds_prepared_winner {
            return PreparationCancelOutcome::NothingPending;
        }

        self.request_cancellation_of_active_jobs();
        self.preparation_stopped_by_user = true;
        // Winner, который ждал allocator gate, никогда не будет начат.
        self.orchestration.prepared = None;
        self.orchestration.target = None;
        self.orchestration.phase = StartupMediaPhase::Idle;
        // Fallback на сохранённый элемент — тоже «открытие»; Stop его отменяет, а не подменяет.
        self.orchestration.restored_fallback_started = true;
        tracing::info!("Стартовая подготовка источника отменена по Stop");
        PreparationCancelOutcome::Cancelled
    }

    /// `true`, пока какой-либо стартовый job подготовки ещё занимает слот.
    fn has_active_preparation_job(&self) -> bool {
        self.yt_dlp_startup_job.is_some()
            || self.direct_media_startup_job.is_some()
            || self.native_hls_startup_job.is_some()
            || self.native_dash_startup_job.is_some()
            || self.native_hds_startup_job.is_some()
            || self.native_smooth_startup_job.is_some()
            || self.local_startup_job.is_some()
    }

    /// Поднимает кооперативную отмену у каждого живого job-а, не дожидаясь выхода worker-а.
    fn request_cancellation_of_active_jobs(&self) {
        if let Some(job) = self.yt_dlp_startup_job.as_ref() {
            job.cancellation_requested.store(true, Ordering::Release);
            job.source_cancellation.cancel();
        }
        if let Some(job) = self.direct_media_startup_job.as_ref() {
            job.cancellation_requested.store(true, Ordering::Release);
        }
        if let Some(job) = self.native_hls_startup_job.as_ref() {
            job.cancellation_requested.store(true, Ordering::Release);
            job.source_cancellation.cancel();
        }
        if let Some(job) = self.native_dash_startup_job.as_ref() {
            job.cancellation_requested.store(true, Ordering::Release);
            job.source_cancellation.cancel();
        }
        if let Some(job) = self.native_hds_startup_job.as_ref() {
            job.cancellation_requested.store(true, Ordering::Release);
            job.source_cancellation.cancel();
        }
        if let Some(job) = self.native_smooth_startup_job.as_ref() {
            job.cancellation_requested.store(true, Ordering::Release);
            job.source_cancellation.cancel();
        }
        if let Some(job) = self.local_startup_job.as_ref() {
            // Тот же intent-метод, что у кнопки Open: подготовка отменяется, диалога тут нет.
            let _outcome = job.cancel_preparation(PreparationCancelCause::UserStop);
        }
    }

    /// После Stop освобождает слоты завершившихся job-ов, молча отбрасывая их итог.
    ///
    /// Итог (успех, ошибка сети, «завершился без результата» после отмены) не превращается
    /// ни в winner, ни в уведомление, ни в бейдж строки. Возвращает `true`, если какой-то
    /// слот освобождён. Не требует renderer-а и не блокируется.
    pub(super) fn discard_stopped_preparation_jobs(&mut self) -> bool {
        let mut changed = false;
        changed |= discard_if_finished(&mut self.yt_dlp_startup_job, |job| {
            job.try_take_result().is_some()
        });
        changed |= discard_if_finished(&mut self.direct_media_startup_job, |job| {
            job.try_take_result().is_some()
        });
        changed |= discard_if_finished(&mut self.native_hls_startup_job, |job| {
            job.try_take_result().is_some()
        });
        changed |= discard_if_finished(&mut self.native_dash_startup_job, |job| {
            job.try_take_result().is_some()
        });
        changed |= discard_if_finished(&mut self.native_hds_startup_job, |job| {
            job.try_take_result().is_some()
        });
        changed |= discard_if_finished(&mut self.native_smooth_startup_job, |job| {
            job.try_take_result().is_some()
        });
        changed |= discard_if_finished(&mut self.local_startup_job, |job| {
            job.drain().completion.is_some()
        });
        changed
    }
}

#[cfg(test)]
mod tests;
