//! Журнал посредника: только stderr и только безопасные поля.
//!
//! Браузер пишет stderr посредника в свой лог (Chrome — в вывод браузера, Firefox
//! — в консоль браузера). Поэтому в журнал попадают только виды событий, размеры и
//! причины: ни заголовков страниц, ни доменов, ни ключей сеанса, ни пропусков, ни
//! ID экстеншенов.
//!
//! События ([`HostEvent`]) отделены от их текста ([`format_event`]) и от вывода
//! ([`StderrLog`]): логика посредника и тесты работают с событиями, а не со строками.

use std::io::Write;

use browser_bridge_protocol::extension_identity::BrowserFamily;

use crate::caller::CallerRejection;
use crate::framing::{FrameReadError, FrameWriteError};
use crate::relay::{
    HostRefusalCause, PlayerReplyKind, ReplyKind, RequestKind, refusal_reason_log_name,
};

/// Префикс каждой строки: в общем логе браузера видно, чья это строка.
const LOG_PREFIX: &str = "fastiplayer-browser-host:";

/// Событие посредника.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// Вызывающий принят.
    CallerAccepted {
        /// Семейство браузера.
        browser: BrowserFamily,
    },
    /// Вызывающий отвергнут — выход без обращения к шине.
    CallerRejected(CallerRejection),
    /// На сообщение браузера отправлен ответ.
    MessageAnswered {
        /// Какое сообщение пришло.
        request: RequestKind,
        /// Его размер, байт.
        request_bytes: usize,
        /// Чем ответили.
        reply: ReplyKind,
    },
    /// Браузер закрыл вход — обычный конец.
    InputClosed,
    /// Рамка от браузера битая — порт закрывается.
    FrameRejected(FrameReadError),
    /// Ответ не записан — браузер ушёл или ответ слишком велик.
    ReplyNotWritten(FrameWriteError),
    /// Свой отказ не удалось записать (по устройству типов невозможно).
    InternalError,
}

/// Куда посредник сообщает о событиях.
pub trait HostLog {
    /// Записывает событие.
    fn record(&mut self, event: &HostEvent);
}

/// Журнал в stderr процесса.
#[derive(Debug, Default)]
pub struct StderrLog;

impl HostLog for StderrLog {
    fn record(&mut self, event: &HostEvent) {
        let line = format!("{LOG_PREFIX} {}\n", format_event(event));
        // Ошибку записи сообщить некуда: stderr — единственный канал журнала, а
        // закрытый stderr (браузер не читает его или уже ушёл) не повод менять
        // поведение посредника. `eprintln!` здесь нельзя: на закрытом stderr он
        // паникует (ловушка сессии 03e, `mem:panic-invariant-policy`).
        let _ignored_log_write_failure = std::io::stderr().lock().write_all(line.as_bytes());
    }
}

/// Текст события для журнала.
#[must_use]
pub fn format_event(event: &HostEvent) -> String {
    match event {
        HostEvent::CallerAccepted { browser } => {
            format!("caller accepted browser={}", browser_log_name(*browser))
        }
        HostEvent::CallerRejected(rejection) => format!("caller rejected: {rejection}"),
        HostEvent::MessageAnswered {
            request,
            request_bytes,
            reply,
        } => format!(
            "answered request={} bytes={request_bytes} reply={}",
            request.log_name(),
            reply_log_text(reply)
        ),
        HostEvent::InputClosed => "input closed, exiting".to_owned(),
        HostEvent::FrameRejected(error) => format!("broken frame, closing the port: {error}"),
        HostEvent::ReplyNotWritten(error) => format!("reply not written, exiting: {error}"),
        HostEvent::InternalError => "internal error: own refusal not encodable".to_owned(),
    }
}

/// Имя семейства браузеров для журнала.
const fn browser_log_name(browser: BrowserFamily) -> &'static str {
    match browser {
        BrowserFamily::Chromium => "chromium",
        BrowserFamily::Firefox => "firefox",
    }
}

/// Текст ответа для журнала: вид ответа плеера или причина своего отказа.
fn reply_log_text(reply: &ReplyKind) -> String {
    match reply {
        ReplyKind::FromPlayer(PlayerReplyKind::HelloAccepted) => "player:hello_accepted".to_owned(),
        ReplyKind::FromPlayer(PlayerReplyKind::SessionGranted) => {
            "player:session_granted".to_owned()
        }
        ReplyKind::FromPlayer(PlayerReplyKind::Refused(reason)) => {
            format!("player:refused:{}", refusal_reason_log_name(reason))
        }
        ReplyKind::RefusedByHost(HostRefusalCause::MalformedMessage(kind)) => {
            format!("host:refused:malformed_message:{kind:?}")
        }
        ReplyKind::RefusedByHost(HostRefusalCause::Door(failure)) => {
            format!("host:refused:{}", failure.log_name())
        }
        ReplyKind::RefusedByHost(HostRefusalCause::InvalidPlayerReply(kind)) => {
            format!("host:refused:invalid_player_reply:{kind:?}")
        }
    }
}

#[cfg(test)]
mod tests;
