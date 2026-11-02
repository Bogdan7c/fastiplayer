//! Рамки native messaging: 4 байта длины в порядке байт машины + JSON в UTF-8.
//!
//! Формат одинаков в обе стороны (документация Chrome и MDN). Пределы разные:
//! - от браузера — не больше [`MAX_MESSAGE_FROM_BROWSER_BYTES`] (128 КиБ, предел
//!   JSON протокола моста). Chrome пропустил бы и 64 МиБ, но наши сообщения входа
//!   крошечные; больше — значит, на том конце не наш экстеншен или он сломан;
//! - к браузеру — не больше [`MAX_MESSAGE_TO_BROWSER_BYTES`] (1 МиБ): на байт
//!   больше браузер закрывает порт (проверено в разведке 00).
//!
//! Битая рамка — ошибка, после которой поток байтов уже не разрезать на сообщения
//! заново. Поэтому читатель только сообщает её вид, а решение «закрыть порт»
//! принимает [`crate::host`].

use std::io::{self, Read, Write};

use browser_bridge_protocol::limits::MAX_JSON_MESSAGE_BYTES;
use thiserror::Error;

/// Предел сообщения от браузера, байт.
pub const MAX_MESSAGE_FROM_BROWSER_BYTES: usize = MAX_JSON_MESSAGE_BYTES;

/// Предел сообщения к браузеру, байт (1 048 576 — ровно предел Chrome).
pub const MAX_MESSAGE_TO_BROWSER_BYTES: usize = 1024 * 1024;

/// Размер префикса длины.
const LENGTH_PREFIX_BYTES: usize = 4;

/// Что прочитано из входа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomingFrame {
    /// Целое сообщение (может быть пустым: нулевая длина — допустимая рамка,
    /// а «пустой JSON» отвергает уже разбор протокола).
    Message(Vec<u8>),
    /// Браузер закрыл вход ровно между рамками: порт закрыт, вкладка ушла
    /// или браузер завершился.
    InputClosed,
}

/// Почему рамку нельзя прочитать.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FrameReadError {
    /// Вход кончился посреди префикса длины.
    #[error("input ended inside the length prefix after {received} of 4 bytes")]
    TruncatedLength {
        /// Сколько байт префикса пришло.
        received: usize,
    },
    /// Объявленная длина больше предела: тело даже не читается.
    #[error("message declares {declared} bytes, the limit is {limit}")]
    TooLarge {
        /// Объявленная длина.
        declared: u32,
        /// Предел.
        limit: usize,
    },
    /// Вход кончился посреди тела сообщения.
    #[error("input ended inside a message after {received} of {declared} bytes")]
    TruncatedMessage {
        /// Объявленная длина.
        declared: usize,
        /// Сколько байт тела пришло.
        received: usize,
    },
    /// Чтение входа не удалось (не конец входа, а ошибка ОС).
    #[error("reading input failed: {kind}")]
    Read {
        /// Вид ошибки ОС.
        kind: io::ErrorKind,
    },
}

/// Читатель рамок из входа браузера.
#[derive(Debug)]
pub struct FrameReader<Input> {
    input: Input,
    limit: usize,
}

impl<Input: Read> FrameReader<Input> {
    /// Читатель с пределом сообщения от браузера.
    pub fn new(input: Input) -> Self {
        Self::with_limit(input, MAX_MESSAGE_FROM_BROWSER_BYTES)
    }

    /// Читатель с явным пределом (для тестов пределов).
    pub fn with_limit(input: Input, limit: usize) -> Self {
        Self { input, limit }
    }

    /// Читает следующую рамку.
    ///
    /// Вход читается ровно на размер рамки: за телом не заглядываем, а при
    /// превышении предела тело не читается совсем — память не зависит от того,
    /// что объявила другая сторона.
    ///
    /// # Errors
    /// Обрыв посреди рамки, превышение предела или ошибка чтения ОС.
    pub fn read_frame(&mut self) -> Result<IncomingFrame, FrameReadError> {
        let Some(declared) = self.read_length_prefix()? else {
            return Ok(IncomingFrame::InputClosed);
        };
        let length = usize::try_from(declared)
            .ok()
            .filter(|length| *length <= self.limit)
            .ok_or(FrameReadError::TooLarge {
                declared,
                limit: self.limit,
            })?;
        self.read_body(length).map(IncomingFrame::Message)
    }

    /// Префикс длины; `None` — вход закрыт до первого байта рамки.
    fn read_length_prefix(&mut self) -> Result<Option<u32>, FrameReadError> {
        let mut prefix = [0_u8; LENGTH_PREFIX_BYTES];
        let mut received = 0;
        while received < LENGTH_PREFIX_BYTES {
            match self.input.read(&mut prefix[received..]) {
                Ok(0) if received == 0 => return Ok(None),
                Ok(0) => return Err(FrameReadError::TruncatedLength { received }),
                Ok(count) => received += count,
                // Сигнал прервал системный вызов — это не ошибка входа, читаем снова.
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(FrameReadError::Read { kind: error.kind() }),
            }
        }
        // Порядок байт машины — так требует протокол native messaging.
        Ok(Some(u32::from_ne_bytes(prefix)))
    }

    /// Тело ровно `length` байт (`length` уже не больше предела).
    fn read_body(&mut self, length: usize) -> Result<Vec<u8>, FrameReadError> {
        let mut body = Vec::with_capacity(length);
        // `take` не даёт прочитать байты следующей рамки; `read_to_end` сам
        // повторяет чтение после `Interrupted`.
        let limit = u64::try_from(length).unwrap_or(u64::MAX);
        (&mut self.input)
            .take(limit)
            .read_to_end(&mut body)
            .map_err(|error| FrameReadError::Read { kind: error.kind() })?;
        if body.len() < length {
            return Err(FrameReadError::TruncatedMessage {
                declared: length,
                received: body.len(),
            });
        }
        Ok(body)
    }
}

/// Почему ответ браузеру не записан.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FrameWriteError {
    /// Ответ длиннее предела браузера: браузер закрыл бы порт.
    #[error("reply of {length} bytes exceeds the browser limit of {limit}")]
    TooLarge {
        /// Длина ответа.
        length: usize,
        /// Предел.
        limit: usize,
    },
    /// Запись не удалась: обычно браузер уже закрыл порт.
    #[error("writing the reply failed: {kind}")]
    Write {
        /// Вид ошибки ОС.
        kind: io::ErrorKind,
    },
}

/// Пишет одну рамку и сразу проталкивает её браузеру.
///
/// Префикс и тело уходят одним буфером: браузер не увидит рамку по частям, даже
/// если вывод буферизуется построчно.
///
/// # Errors
/// Ответ больше предела браузера или ошибка записи.
pub fn write_frame<Output: Write>(
    output: &mut Output,
    message: &str,
) -> Result<(), FrameWriteError> {
    let too_large = FrameWriteError::TooLarge {
        length: message.len(),
        limit: MAX_MESSAGE_TO_BROWSER_BYTES,
    };
    if message.len() > MAX_MESSAGE_TO_BROWSER_BYTES {
        return Err(too_large);
    }
    let length = u32::try_from(message.len()).map_err(|_| too_large)?;
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + message.len());
    frame.extend_from_slice(&length.to_ne_bytes());
    frame.extend_from_slice(message.as_bytes());
    output
        .write_all(&frame)
        .and_then(|()| output.flush())
        .map_err(|error| FrameWriteError::Write { kind: error.kind() })
}

#[cfg(test)]
mod tests;
