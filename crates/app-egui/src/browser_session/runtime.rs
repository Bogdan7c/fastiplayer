//! Главный цикл процесса браузерного режима (без окна, сессия 02).
//!
//! Цикл владеет тремя вещами и ничем больше:
//! - стойкой входа [`EntryDesk`]: каждый звонок посредника получает ровно один ответ;
//! - машиной «когда выходить» [`ProcessLifecycle`] (решение 56);
//! - просьбой «пауза» обычному плееру при старте и замене сеанса (решения 41, 48).
//!
//! Сеанс, сервер WebSocket и байты живут в `browser-bridge-session`; сюда приходят
//! только события через ящик.

use std::sync::Arc;
use std::time::Instant;

use browser_bridge_protocol::GrantOutcome;
use browser_bridge_session::{
    BrowserSessionPolicy, EntryCaller, EntryDesk, EntryEvent, ExitCause, LifecycleVerdict,
    ProcessLifecycle, SessionLifecycleEvent, SessionRef,
};
use desktop_integration::BrowserEntryDelivery;
use tracing::{debug, error, info};

use super::inbox::{BrowserSessionInbox, InboxBatch, ParkingWake};
use super::normal_player_pause::NormalPlayerPause;

/// Чем закончилась работа цикла (в лог и в код выхода).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeFinish {
    /// Почему процесс выходит.
    pub(crate) cause: ExitCause,
    /// Сколько сообщений страницы цикл получил (в 02 они только считаются).
    pub(crate) page_messages_received: u64,
}

/// Главный цикл процесса браузерного режима.
pub(super) struct BrowserSessionRuntime {
    desk: EntryDesk,
    lifecycle: ProcessLifecycle,
    inbox: Arc<BrowserSessionInbox>,
    normal_player: Box<dyn NormalPlayerPause>,
    page_messages_received: u64,
}

impl BrowserSessionRuntime {
    /// Время старта процесса отсчитывается отсюда: с него идёт срок «никто так и
    /// не открыл сеанс» (`idle_exit_after_start`).
    pub(super) fn new(
        desk: EntryDesk,
        policy: &BrowserSessionPolicy,
        inbox: Arc<BrowserSessionInbox>,
        normal_player: Box<dyn NormalPlayerPause>,
    ) -> Self {
        Self {
            desk,
            lifecycle: ProcessLifecycle::new(Instant::now(), policy.idle_exit_after_start),
            inbox,
            normal_player,
            page_messages_received: 0,
        }
    }

    /// Крутится, пока машина «когда выходить» не скажет «выходим».
    pub(super) fn run_until_exit(mut self, wake: &ParkingWake) -> RuntimeFinish {
        loop {
            match self.step(Instant::now()) {
                LifecycleVerdict::Exit(cause) => return self.finish(cause),
                LifecycleVerdict::KeepRunning { wake_at } => wake.wait(wake_at),
            }
        }
    }

    /// Один проход: забрать ящик, обработать, спросить машину.
    ///
    /// Вынесен отдельно, чтобы тесты шли по шагам без потоков и сна.
    pub(super) fn step(&mut self, now: Instant) -> LifecycleVerdict {
        let batch = self.inbox.take_batch();
        self.process_batch(batch, now);
        self.lifecycle.verdict(now)
    }

    /// Дожидается начатых просьб «пауза» и отдаёт итог.
    pub(super) fn finish(mut self, cause: ExitCause) -> RuntimeFinish {
        self.normal_player.finish();
        RuntimeFinish {
            cause,
            page_messages_received: self.page_messages_received,
        }
    }

    /// Порядок важен: звонки входа раньше событий сеанса. Событие сеанса (например,
    /// `Attached`) возможно только после ответа с пропуском, а ответ уходит здесь же,
    /// поэтому машина всегда узнаёт об открытии сеанса раньше, чем о его жизни.
    fn process_batch(&mut self, batch: InboxBatch, now: Instant) {
        for delivery in batch.entry_deliveries {
            self.answer_entry_call(delivery, now);
        }
        for event in &batch.lifecycle_events {
            log_lifecycle_event(event);
            self.lifecycle.on_lifecycle_event(event);
        }
        // Источник байтов и окно появятся в 03–04; пока сообщения только считаются.
        // Содержимое в лог не идёт: в нём бывают названия серий и озвучек (инвариант 7).
        let received = batch.page_messages.len() as u64;
        if received > 0 {
            self.page_messages_received += received;
            debug!(received, "Page session messages received");
        }
    }

    fn answer_entry_call(&mut self, delivery: BrowserEntryDelivery, now: Instant) {
        let (request, reply) = delivery.into_parts();
        let caller = match EntryCaller::new(request.caller.as_str()) {
            Ok(caller) => caller,
            Err(caller_error) => {
                // Имя отправителя на шине всегда короткое и печатное; если нет — это
                // поломка транспорта. Слот уничтожается без ответа: звонящий получит
                // `ShuttingDown`, а не вечное ожидание.
                error!(%caller_error, "Browser entry call from an unusable caller id");
                drop(reply);
                return;
            }
        };
        let outcome = self.desk.handle(&caller, request.message.as_bytes(), now);
        reply.send(outcome.reply_json);

        if let Some(EntryEvent::SessionOpened {
            key,
            instance,
            intent,
            outcome: grant,
            ..
        }) = outcome.event
        {
            // Машина сверяет сеанс по паре «ключ + экземпляр»: конец прежнего
            // экземпляра с тем же ключом (истёк срок возврата) не закроет новый.
            self.lifecycle
                .on_session_opened(&SessionRef { key, instance });
            // Домен и заголовок страницы в лог НЕ пишутся: журнал systemd лежит на
            // диске, а браузерный режим ничего не хранит о просмотре (инвариант 7,
            // инкогнито — решение 41). Они нужны только окну (04).
            info!(?intent, ?grant, "Browser session opened");
            if needs_normal_player_pause(grant) {
                self.normal_player.request_pause();
            }
        }
    }
}

/// Пауза нужна, когда начинается новое видео: новый сеанс или замена из другой
/// вкладки (человек мог снять обычный плеер с паузы между ними). Повтор того же
/// сеанса — это то же видео, повторная просьба была бы лишним вмешательством.
fn needs_normal_player_pause(grant: GrantOutcome) -> bool {
    match grant {
        GrantOutcome::Started | GrantOutcome::ReplacedPrevious => true,
        GrantOutcome::ReusedExisting => false,
    }
}

fn log_lifecycle_event(event: &SessionLifecycleEvent) {
    match event {
        SessionLifecycleEvent::Attached { .. } => info!("Browser session attached"),
        SessionLifecycleEvent::ConnectionLost { .. } => {
            info!("Browser session connection lost; waiting for the extension to come back");
        }
        SessionLifecycleEvent::Ended { reason, .. } => info!(?reason, "Browser session ended"),
    }
}

#[cfg(test)]
mod tests;
