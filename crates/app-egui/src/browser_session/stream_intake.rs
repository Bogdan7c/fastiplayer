//! Поток приёма байтов `browser-stream`: единая очередь → переходник → ящик.
//!
//! Поток ждёт данных на очереди ([`OrderedSessionQueue::wait_batch`]), прогоняет
//! их через [`ProtocolTranslator`] строго по порядку и кладёт уведомления в ящик
//! главного цикла одной пачкой (одно пробуждение на пачку). Писатель потока
//! существует только здесь: главный цикл видит сторону открытия и монитор.
//!
//! Остановка ([`StreamIntake::shut_down`]) будит поток через очередь и ждёт его с
//! потолком: зависший разбор не должен держать выход процесса. Не дождались —
//! поток отпускается (он сам выйдет, проснувшись), это видно в итоге.
//!
//! Паника при разборе одного входа (дефект переходника или источника) не убивает
//! поток молча: она ловится на этом входе, в журнал пишется ошибка, а экземпляр
//! сеанса, чей вход разбирался, сразу закрывается
//! ([`ProtocolTranslator::close_after_internal_failure`]). Иначе страница слала бы
//! байты в мёртвый поток, а плеер ждал бы их до переполнения очереди. Поток живёт
//! дальше: следующие сеансы разбираются с чистого состояния.

use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use tracing::{error, info, warn};

use super::inbox::BrowserSessionInbox;
use super::intake_notice::IntakeNotice;
use super::stream_queue::{OrderedSessionQueue, QueueTake, SessionInput};
use super::translator::stream_port::StreamFactory;
use super::translator::{ProtocolTranslator, TranslatorCounters};

/// Имя потока (видно в отладчике и `top -H`).
const THREAD_NAME: &str = "browser-stream";

/// Итог остановки потока приёма.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntakeShutdown {
    /// Поток вышел; его итоговые счётчики.
    Joined(TranslatorCounters),
    /// Поток не вышел за потолок и отпущен (разбор завис внутри источника).
    Detached,
    /// Поток упал с паникой (дефект; данные сеанса потеряны).
    Panicked,
}

/// Запущенный поток приёма.
pub(crate) struct StreamIntake {
    /// Очередь, через которую поток будится и останавливается.
    queue: Arc<OrderedSessionQueue>,
    /// Ручка потока.
    thread: JoinHandle<TranslatorCounters>,
    /// Сигнал «поток вышел из цикла» — ради ожидания с потолком.
    finished: mpsc::Receiver<()>,
}

impl StreamIntake {
    /// Запускает поток приёма над очередью; уведомления — в `inbox`.
    ///
    /// # Errors
    /// ОС не дала создать поток.
    pub(crate) fn start<F>(
        queue: Arc<OrderedSessionQueue>,
        inbox: Arc<BrowserSessionInbox>,
        translator: ProtocolTranslator<F>,
    ) -> io::Result<Self>
    where
        F: StreamFactory + 'static,
    {
        let (finished_sender, finished) = mpsc::sync_channel(1);
        let thread_queue = Arc::clone(&queue);
        let thread = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || {
                let counters = run_intake_loop(&thread_queue, &inbox, translator);
                // Получатель мог уже уйти (остановка не дождалась) — это не ошибка.
                let _ = finished_sender.send(());
                counters
            })?;
        Ok(Self {
            queue,
            thread,
            finished,
        })
    }

    /// Останавливает поток и ждёт его не дольше `join_timeout`.
    pub(crate) fn shut_down(self, join_timeout: Duration) -> IntakeShutdown {
        self.queue.shut_down();
        match self.finished.recv_timeout(join_timeout) {
            // Отправитель ушёл без сигнала — поток упал; `join` это покажет.
            Ok(()) | Err(RecvTimeoutError::Disconnected) => match self.thread.join() {
                Ok(counters) => IntakeShutdown::Joined(counters),
                Err(_panic) => {
                    warn!("Browser stream intake thread panicked");
                    IntakeShutdown::Panicked
                }
            },
            Err(RecvTimeoutError::Timeout) => {
                warn!(
                    timeout_ms = u64::try_from(join_timeout.as_millis()).unwrap_or(u64::MAX),
                    "Browser stream intake thread did not stop in time; detaching it"
                );
                IntakeShutdown::Detached
            }
        }
    }
}

/// Цикл потока: забрать пачку, перевести, отдать уведомления.
fn run_intake_loop<F: StreamFactory>(
    queue: &OrderedSessionQueue,
    inbox: &BrowserSessionInbox,
    mut translator: ProtocolTranslator<F>,
) -> TranslatorCounters {
    let mut notices = Vec::new();
    while let QueueTake::Batch(inputs) = queue.wait_batch() {
        for input in inputs {
            apply_guarded(&mut translator, input, &mut notices);
        }
        if !notices.is_empty() {
            let delivery = inbox.post_intake_notices(notices.drain(..));
            if delivery.dropped_after_close > 0 {
                info!(
                    dropped = delivery.dropped_after_close,
                    "Stream notices arrived after the inbox closed"
                );
            }
        }
    }
    let counters = translator.counters();
    info!(?counters, "Browser stream intake stopped");
    counters
}

/// Один вход через переходник; паника разбора закрывает экземпляр этого входа.
///
/// `AssertUnwindSafe`: после паники переходнику не верим — состояние упавшего
/// экземпляра целиком заменяется на «закрывается» (писатель бросается), а
/// остальное его состояние — только счётчики и фабрика без состояния разбора.
fn apply_guarded<F: StreamFactory>(
    translator: &mut ProtocolTranslator<F>,
    input: SessionInput,
    notices: &mut Vec<IntakeNotice>,
) {
    let session = input.session().clone();
    let applied = catch_unwind(AssertUnwindSafe(|| translator.apply(input, notices)));
    if applied.is_err() {
        // Текст паники сюда не копируется: в нём могли оказаться данные страницы.
        error!(
            instance = session.instance.0,
            "Browser stream translator panicked; closing the session instance"
        );
        translator.close_after_internal_failure(&session, notices);
    }
}

#[cfg(test)]
mod tests;
