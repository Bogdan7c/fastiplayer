//! Фоновый поток задачи загрузки субтитров и доставка её результата владельцу.
//!
//! Повторяет проверенную схему `LocalFileOpenJob`: результат уходит через владельческий
//! mailbox (`owner_mailbox`) с wake-событием, поэтому окно перерисуется, как только
//! результат готов (в том числе на паузе). Отмена кооперативная (`AtomicBool`): флаг
//! опрашивается между шагами задачи и внутри поиска файлов.
//!
//! Владение потоком: `Drop` отменяет задачу и ждёт её завершения. Ожидание короткое —
//! каждый шаг задачи ограничен лимитами, а отмена проверяется между шагами. Скрытого
//! detach нет: «ушедшие» задачи хранит владелец до завершения и дожимает их в `Drop`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use source_core::CancellationToken;

use crate::app_wake::{AppWakePort, OwnerMailboxReceiver, WakeDelivery, owner_mailbox};

use super::sidecar_job::{
    SidecarJobOutcome, SidecarJobRequest, SubtitleLoadFailure, run_sidecar_job,
};

/// Имя потока для диагностики.
const SIDECAR_JOB_THREAD_NAME: &str = "subtitle-sidecar-load";

/// Что владелец увидел при опросе задачи.
#[expect(
    clippy::large_enum_variant,
    reason = "значение живёт мгновение и один раз переезжает через mailbox; Box только усложнил бы доступ"
)]
pub(super) enum SidecarJobPoll {
    /// Результата ещё нет.
    Pending,
    /// Задача завершилась (результат выдаётся ровно один раз).
    Finished(SidecarJobOutcome),
}

/// Запущенная фоновая задача загрузки субтитров.
pub(super) struct SidecarJob {
    mailbox_receiver: OwnerMailboxReceiver<(), SidecarJobOutcome>,
    join_handle: Option<JoinHandle<()>>,
    cancellation_requested: Arc<AtomicBool>,
    /// Токен блокирующей сетевой части задачи (у web-загрузки): без него `cancel`/`Drop`
    /// ждали бы конца HTTP-запроса. `None` у задач без сети.
    network_cancellation: Option<CancellationToken>,
}

impl SidecarJob {
    /// Запускает поток задачи. Ошибка создания потока возвращается типизированно.
    pub(super) fn spawn(
        request: SidecarJobRequest,
        wake_port: AppWakePort,
    ) -> Result<Self, SubtitleLoadFailure> {
        let (mailbox_publisher, mailbox_receiver) = owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let network_cancellation = request.task.cancellation_token().cloned();
        let worker_cancellation = Arc::clone(&cancellation_requested);
        let join_handle = thread::Builder::new()
            .name(SIDECAR_JOB_THREAD_NAME.to_owned())
            .spawn(move || {
                let outcome =
                    run_sidecar_job(&request, &|| worker_cancellation.load(Ordering::Acquire));
                match mailbox_publisher.publish_completion(outcome) {
                    Ok(WakeDelivery::EventLoopClosed) => {
                        tracing::debug!("Event loop closed; subtitle result will not retry wake");
                    }
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(error) => {
                        tracing::error!(?error, "Subtitle job published its result twice");
                    }
                }
            })
            .map_err(|error| SubtitleLoadFailure::WorkerStartFailed { kind: error.kind() })?;
        Ok(Self {
            mailbox_receiver,
            join_handle: Some(join_handle),
            cancellation_requested,
            network_cancellation,
        })
    }

    /// Просит задачу остановиться (результат будет выброшен владельцем).
    pub(super) fn cancel(&self) {
        self.cancellation_requested.store(true, Ordering::Release);
        if let Some(network_cancellation) = &self.network_cancellation {
            network_cancellation.cancel();
        }
    }

    /// Неблокирующе забирает результат. Поток, умерший без результата (паника), — `WorkerLost`.
    pub(super) fn poll(&self) -> SidecarJobPoll {
        let drain = self.mailbox_receiver.drain();
        if let Some(outcome) = drain.completion {
            return SidecarJobPoll::Finished(outcome);
        }
        if drain.producer_disconnected_without_completion {
            return SidecarJobPoll::Finished(SidecarJobOutcome::Failed(
                SubtitleLoadFailure::WorkerLost,
            ));
        }
        SidecarJobPoll::Pending
    }

    /// Поток уже завершился (можно безопасно освободить без ожидания).
    pub(super) fn is_finished(&self) -> bool {
        self.join_handle
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for SidecarJob {
    fn drop(&mut self) {
        self.cancel();
        let Some(join_handle) = self.join_handle.take() else {
            return;
        };
        if join_handle.join().is_err() {
            tracing::warn!("Subtitle loader thread panicked");
        }
    }
}
