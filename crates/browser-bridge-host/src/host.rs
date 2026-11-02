//! Жизнь посредника: проверить вызывающего, отвечать на рамки до конца входа.
//!
//! Посредник выходит сам:
//! - браузер закрыл вход (порт закрыт, вкладка или браузер ушли) — код 0. Это
//!   обязательно: при закрытии браузера посредника никто не убивает, он получает
//!   только EOF (разведка 00), и без выхода остался бы сиротой;
//! - вызывающий чужой — до любого обращения к шине;
//! - рамка битая — поток байтов уже не разрезать на сообщения, порт закрывается
//!   (браузер видит «native host has exited», причина — в stderr);
//! - ответ не записан — браузер уже не слушает.
//!
//! Состояния, которое нельзя потерять, нет: сеанс живёт в плеере и экстеншене.

use std::ffi::OsString;
use std::io::{Read, Write};

use crate::caller::{CallerRejection, identify_caller};
use crate::door::PlayerDoor;
use crate::framing::{FrameReadError, FrameReader, FrameWriteError, IncomingFrame, write_frame};
use crate::log::{HostEvent, HostLog};
use crate::relay::Relay;

/// Почему посредник завершился.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostExit {
    /// Браузер закрыл вход — обычный конец.
    InputClosed,
    /// Вызывающий не наш экстеншен.
    ForeignCaller(CallerRejection),
    /// Битая рамка от браузера.
    BrokenFrame(FrameReadError),
    /// Ответ браузеру не записан.
    ReplyNotWritten(FrameWriteError),
    /// Свой отказ не удалось записать.
    InternalError,
}

impl HostExit {
    /// Код выхода процесса. Разные причины — разные коды: браузер их не читает,
    /// но по ним видно причину в тестах и при ручной проверке.
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::InputClosed => 0,
            Self::ForeignCaller(_) => 2,
            Self::BrokenFrame(_) => 3,
            Self::ReplyNotWritten(_) => 4,
            Self::InternalError => 5,
        }
    }
}

/// Запускает посредника.
///
/// `arguments` — аргументы без пути к бинарю; `open_door` вызывается только после
/// того, как вызывающий принят (чужой экстеншен не доходит до шины).
pub fn run_host<Input, Output, Door>(
    arguments: &[OsString],
    input: Input,
    mut output: Output,
    open_door: impl FnOnce() -> Door,
    log: &mut impl HostLog,
) -> HostExit
where
    Input: Read,
    Output: Write,
    Door: PlayerDoor,
{
    let caller = match identify_caller(arguments) {
        Ok(caller) => caller,
        Err(rejection) => {
            log.record(&HostEvent::CallerRejected(rejection));
            return HostExit::ForeignCaller(rejection);
        }
    };
    log.record(&HostEvent::CallerAccepted {
        browser: caller.browser(),
    });
    let mut frames = FrameReader::new(input);
    let mut relay = Relay::new(open_door());
    loop {
        let message = match frames.read_frame() {
            Ok(IncomingFrame::Message(message)) => message,
            Ok(IncomingFrame::InputClosed) => {
                log.record(&HostEvent::InputClosed);
                return HostExit::InputClosed;
            }
            Err(error) => {
                log.record(&HostEvent::FrameRejected(error));
                return HostExit::BrokenFrame(error);
            }
        };
        let Ok(reply) = relay.handle(&message) else {
            log.record(&HostEvent::InternalError);
            return HostExit::InternalError;
        };
        if let Err(error) = write_frame(&mut output, &reply.json) {
            log.record(&HostEvent::ReplyNotWritten(error));
            return HostExit::ReplyNotWritten(error);
        }
        log.record(&HostEvent::MessageAnswered {
            request: reply.request,
            request_bytes: message.len(),
            reply: reply.reply,
        });
    }
}

#[cfg(test)]
mod tests;
