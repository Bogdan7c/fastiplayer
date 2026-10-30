//! Главный цикл процесса браузерного режима (без окна, сессии 02 и 03b).
//!
//! Цикл владеет тремя вещами и ничем больше:
//! - стойкой входа [`EntryDesk`]: каждый звонок посредника получает ровно один ответ;
//! - машиной «когда выходить» [`ProcessLifecycle`] (решение 56);
//! - просьбой «пауза» обычному плееру при старте и замене сеанса (решения 41, 48).
//!
//! Сеанс, сервер WebSocket и байты живут в `browser-bridge-session`; сюда приходят
//! только события через ящик. Медиа (поток страницы → плеер → ответы странице) —
//! забота [`SessionMediaPort`] (`super::session_media`): цикл только сообщает ему об
//! открытом сеансе, отдаёт уведомления потока приёма и спрашивает, когда проснуться.
//!
//! Один проход ([`BrowserSessionRuntime::step`]):
//! ```text
//!  ящик ─► звонки входа ─► события жизни ─► уведомления потока приёма ─► media.drive(now)
//!                                                                          │
//!  сон до ближайшего: срок машины жизни │ повтор открытия │ тик часов │ повтор отправки
//!  (или раньше — пробуждение: звонок, событие, байты, новый кадр плеера)
//! ```

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
use super::intake_notice::IntakeNotice;
use super::normal_player_pause::NormalPlayerPause;
use super::session_media::SessionMedia;

/// Медиа-часть так, как её видит главный цикл. Настоящая — [`SessionMedia`];
/// тесты цикла подставляют подделку без плеера.
pub(super) trait SessionMediaPort {
    /// Открыт сеанс (новый, заменивший или повтор того же экземпляра).
    fn on_session_opened(&mut self, session: &SessionRef);
    /// Экземпляр сеанса закончился (сервер сообщил `Ended`).
    fn on_session_ended(&mut self, session: &SessionRef);
    /// Уведомления потока приёма за проход и слитое «легли новые байты».
    fn on_intake(&mut self, notices: Vec<IntakeNotice>, bytes_appended: Option<SessionRef>);
    /// Шаг медиа; возвращает, когда проснуться без внешнего повода.
    fn drive(&mut self, now: Instant) -> Option<Instant>;
}

impl SessionMediaPort for SessionMedia {
    fn on_session_opened(&mut self, session: &SessionRef) {
        Self::on_session_opened(self, session);
    }

    fn on_session_ended(&mut self, session: &SessionRef) {
        Self::on_session_ended(self, session);
    }

    fn on_intake(&mut self, notices: Vec<IntakeNotice>, bytes_appended: Option<SessionRef>) {
        Self::on_intake(self, notices, bytes_appended);
    }

    fn drive(&mut self, now: Instant) -> Option<Instant> {
        Self::drive(self, now)
    }
}

/// Чем закончилась работа цикла (в лог и в код выхода).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeFinish {
    /// Почему процесс выходит.
    ///
    /// Сколько сообщений страницы пришло, здесь не считается: с 03b они идут через
    /// единую очередь в поток приёма, и их счёт — в отчёте потока приёма
    /// (`intake_report` финальной строки процесса).
    pub(crate) cause: ExitCause,
}

/// Главный цикл процесса браузерного режима.
pub(super) struct BrowserSessionRuntime {
    desk: EntryDesk,
    lifecycle: ProcessLifecycle,
    inbox: Arc<BrowserSessionInbox>,
    normal_player: Box<dyn NormalPlayerPause>,
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
        }
    }

    /// Крутится, пока машина «когда выходить» не скажет «выходим».
    ///
    /// Медиа-часть остаётся у вызывающего: её итог (финальная строка журнала плеера)
    /// пишется после остановки сервера и потока приёма.
    pub(super) fn run_until_exit(
        mut self,
        wake: &ParkingWake,
        media: &mut dyn SessionMediaPort,
    ) -> RuntimeFinish {
        loop {
            match self.step(Instant::now(), media) {
                LifecycleVerdict::Exit(cause) => return self.finish(cause),
                LifecycleVerdict::KeepRunning { wake_at } => wake.wait(wake_at),
            }
        }
    }

    /// Один проход: забрать ящик, обработать, шаг медиа, спросить машину.
    ///
    /// Вынесен отдельно, чтобы тесты шли по шагам без потоков и сна. Срок
    /// пробуждения — ближайший из срока машины жизни и срока медиа.
    pub(super) fn step(
        &mut self,
        now: Instant,
        media: &mut dyn SessionMediaPort,
    ) -> LifecycleVerdict {
        let batch = self.inbox.take_batch();
        self.process_batch(batch, now, media);
        let media_wake_at = media.drive(now);
        match self.lifecycle.verdict(now) {
            LifecycleVerdict::KeepRunning { wake_at } => LifecycleVerdict::KeepRunning {
                wake_at: earliest(wake_at, media_wake_at),
            },
            exit @ LifecycleVerdict::Exit(_) => exit,
        }
    }

    /// Дожидается начатых просьб «пауза» и отдаёт итог.
    pub(super) fn finish(mut self, cause: ExitCause) -> RuntimeFinish {
        self.normal_player.finish();
        RuntimeFinish { cause }
    }

    /// Порядок важен: звонки входа раньше событий сеанса. Событие сеанса (например,
    /// `Attached`) возможно только после ответа с пропуском, а ответ уходит здесь же,
    /// поэтому машина всегда узнаёт об открытии сеанса раньше, чем о его жизни.
    ///
    /// Уведомления потока приёма идут после событий жизни: открытие сеанса медиа-часть
    /// узнаёт раньше его данных (данные возможны только после пропуска).
    fn process_batch(&mut self, batch: InboxBatch, now: Instant, media: &mut dyn SessionMediaPort) {
        for delivery in batch.entry_deliveries {
            self.answer_entry_call(delivery, now, media);
        }
        for event in &batch.lifecycle_events {
            log_lifecycle_event(event);
            self.lifecycle.on_lifecycle_event(event);
            if let SessionLifecycleEvent::Ended { session, .. } = event {
                media.on_session_ended(session);
            }
        }
        // С 03b сообщения страницы идут через единую очередь в поток приёма; сюда
        // они попадают, только если ящик сам стоит приёмником сервера (тесты цикла).
        // Содержимое в лог не идёт: в нём бывают названия серий и озвучек (инвариант 7).
        let received = batch.page_messages.len();
        if received > 0 {
            debug!(received, "Page session messages received by the inbox");
        }
        media.on_intake(batch.intake_notices, batch.bytes_appended);
    }

    fn answer_entry_call(
        &mut self,
        delivery: BrowserEntryDelivery,
        now: Instant,
        media: &mut dyn SessionMediaPort,
    ) {
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
            let session = SessionRef { key, instance };
            self.lifecycle.on_session_opened(&session);
            media.on_session_opened(&session);
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

/// Ближайший из двух сроков (`None` — срока нет).
fn earliest(first: Option<Instant>, second: Option<Instant>) -> Option<Instant> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (first, second) => first.or(second),
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
