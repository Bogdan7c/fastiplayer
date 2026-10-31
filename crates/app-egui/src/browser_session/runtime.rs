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
//!  машина выхода: «доигрываем» ─► media.playback_drain(now) ─► «доиграно» ─► выход
//!                                                                          │
//!  сон до ближайшего: срок машины жизни │ повтор открытия │ тик часов │ повтор отправки
//!  (или раньше — пробуждение: звонок, событие, байты, запрос байтов, новый кадр плеера)
//! ```
//!
//! «Доигрываем» (решение 42, 03c): после `tab_gone` и пропажи экстеншена машина не
//! выходит, а ждёт ответа медиа-части [`SessionMediaPort::playback_drain`]. Наружу из
//! [`BrowserSessionRuntime::step`] состояние «доигрываем» не выходит: пока доигрываем,
//! это `KeepRunning` со сроком медиа-части, а когда доиграно — `Exit`.

use std::sync::Arc;
use std::time::Instant;

use browser_bridge_protocol::GrantOutcome;
use browser_bridge_session::{
    BrowserSessionPolicy, EntryCaller, EntryDesk, EntryEvent, ExitCause, LifecycleVerdict,
    ProcessLifecycle, SessionEndReason, SessionLifecycleEvent, SessionRef,
};
use desktop_integration::BrowserEntryDelivery;
use tracing::{debug, error, info};

use super::inbox::{BrowserSessionInbox, InboxBatch};
use super::intake_notice::IntakeNotice;
use super::normal_player_pause::NormalPlayerPause;
use super::session_media::{DrainProgress, SessionMedia};

/// Через сколько просыпаться, если машина выхода вдруг вернёт «доигрываем» мимо
/// [`BrowserSessionRuntime::step`] (так не бывает; страховка от зависания цикла).
/// Нужна любому циклу, который гоняет шаги: безоконному и окну.
pub(super) const DRAIN_FALLBACK_WAKE: std::time::Duration = std::time::Duration::from_millis(250);

/// Медиа-часть так, как её видит главный цикл. Настоящая — [`SessionMedia`];
/// тесты цикла подставляют подделку без плеера.
pub(super) trait SessionMediaPort {
    /// Новая вкладка вытеснила `replaced` (решения 40, 127): последнее сообщение старой
    /// вкладке. Цикл зовёт это в том же шаге и ДО [`Self::on_session_opened`] нового
    /// сеанса — открытие ставит видео на паузу и забывает позицию старой вкладки.
    fn bid_farewell_to_replaced(&mut self, replaced: &SessionRef);
    /// Открыт сеанс (новый, заменивший или повтор того же экземпляра).
    fn on_session_opened(&mut self, session: &SessionRef);
    /// Экземпляр сеанса закончился (сервер сообщил `Ended` с причиной).
    fn on_session_ended(&mut self, session: &SessionRef, reason: SessionEndReason);
    /// Уведомления потока приёма за проход и слитое «легли новые байты».
    fn on_intake(&mut self, notices: Vec<IntakeNotice>, bytes_appended: Option<SessionRef>);
    /// Шаг медиа; возвращает, когда проснуться без внешнего повода.
    fn drive(&mut self, now: Instant) -> Option<Instant>;
    /// Доиграно ли полученное после конца сеанса (зовётся, только пока машина выхода
    /// в состоянии «доигрываем»).
    fn playback_drain(&mut self, now: Instant) -> DrainProgress;
}

impl SessionMediaPort for SessionMedia {
    fn bid_farewell_to_replaced(&mut self, replaced: &SessionRef) {
        Self::bid_farewell_to_replaced(self, replaced);
    }

    fn on_session_opened(&mut self, session: &SessionRef) {
        Self::on_session_opened(self, session);
    }

    fn on_session_ended(&mut self, session: &SessionRef, reason: SessionEndReason) {
        Self::on_session_ended(self, session, reason);
    }

    fn on_intake(&mut self, notices: Vec<IntakeNotice>, bytes_appended: Option<SessionRef>) {
        Self::on_intake(self, notices, bytes_appended);
    }

    fn drive(&mut self, now: Instant) -> Option<Instant> {
        Self::drive(self, now)
    }

    fn playback_drain(&mut self, now: Instant) -> DrainProgress {
        Self::playback_drain(self, now)
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
        let verdict = match self.lifecycle.verdict(now) {
            LifecycleVerdict::AwaitPlaybackDrain => self.settle_playback_drain(now, media),
            other => other,
        };
        match verdict {
            LifecycleVerdict::KeepRunning { wake_at } => LifecycleVerdict::KeepRunning {
                wake_at: earliest(wake_at, media_wake_at),
            },
            LifecycleVerdict::Exit(_) | LifecycleVerdict::AwaitPlaybackDrain => verdict,
        }
    }

    /// Машина ждёт «доиграно»: спрашиваем медиа-часть. Доиграно — сообщаем машине и
    /// берём её решение «выход»; ещё играем — просыпаться к сроку медиа-части.
    fn settle_playback_drain(
        &mut self,
        now: Instant,
        media: &mut dyn SessionMediaPort,
    ) -> LifecycleVerdict {
        match media.playback_drain(now) {
            DrainProgress::Draining { wake_at } => LifecycleVerdict::KeepRunning {
                wake_at: Some(wake_at),
            },
            // Окно на паузе ждёт человека: будить по времени незачем, его действие само
            // разбудит цикл окна (событие winit).
            DrainProgress::AwaitingViewer => LifecycleVerdict::KeepRunning { wake_at: None },
            DrainProgress::Drained(end) => {
                // Одна строка без приватных данных: по ней видно, чем кончилось.
                info!(class = %end.log_label(), "Playback drain finished");
                let outcome = self.lifecycle.on_playback_drained();
                debug!(?outcome, "Process lifecycle told the playback drained");
                self.lifecycle.verdict(now)
            }
        }
    }

    /// Зритель закрыл окно, пока сеанс доигрывается (страницы уже нет): закрытие окна
    /// = «доиграно» по воле зрителя (04b). Возвращает решение машины — при доигрывании
    /// это выход с причиной конца сеанса; если машина не доигрывала, её решение не
    /// меняется (итог виден в журнале).
    pub(super) fn finish_drain_for_viewer(&mut self, now: Instant) -> LifecycleVerdict {
        let outcome = self.lifecycle.on_playback_drained();
        info!(class = %"viewer_closed", ?outcome, "Playback drain finished");
        self.lifecycle.verdict(now)
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
            if let SessionLifecycleEvent::Ended { session, reason } = event {
                media.on_session_ended(session, *reason);
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
            replaced,
            ..
        }) = outcome.event
        {
            // Прощание старой вкладке — раньше открытия нового сеанса в медиа-части:
            // открытие ставит видео на паузу и забывает загруженное поколение, а T —
            // позиция именно этого видео (решение 127).
            if let Some(replaced) = replaced.as_ref() {
                media.bid_farewell_to_replaced(replaced);
            }
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
