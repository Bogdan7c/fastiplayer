//! Поток байтов, который сам повторяет системный вызов, прерванный сигналом.
//!
//! Зачем (сессия 16d2, сон машины посреди просмотра). Перед сном ядро Linux
//! «будит» каждый поток процесса, чтобы его заморозить. Чтение или запись сокета
//! с тайм-аутом (`SO_RCVTIMEO` / `SO_SNDTIMEO`; у сервера сеанса это шаг опроса
//! `connection_poll_interval` и потолок записи `write_timeout`) ядро в этот момент
//! само не перезапускает: вызов возвращается с `EINTR`, в Rust это
//! `ErrorKind::Interrupted`. Сокет при этом цел. Замер на T480s: в миг оттаивания
//! чтение сокета вернуло `Interrupted`, а следующие чтения того же сокета шли без
//! потерь.
//!
//! До 16d2 сервер принимал `Interrupted` за уход собеседника и после каждого
//! пробуждения сам рвал живую связь с экстеншеном: окно плеера закрывалось
//! («Lost connection to the browser»), видео оставалось на странице. Сама
//! `tungstenite` такие вызовы не повторяет: рукопожатие считает их провалом,
//! чтение и запись отдают ошибку наверх.
//!
//! Повтор здесь тот же, что делают `Read::read_exact` и `Write::write_all`
//! стандартной библиотеки. Прерванный вызов не передал ни байта (иначе ядро
//! вернуло бы число переданных байт), поэтому повтор не портит состояние
//! `tungstenite`. Каждый повтор заново ждёт свой тайм-аут сокета: шаг опроса,
//! потолок записи и обратное давление остаются прежними.

use std::io::{self, ErrorKind, Read, Write};

/// Обёртка потока: `read`, `write` и `flush` повторяются, пока их прерывает
/// сигнал; любая другая ошибка и любой результат уходят наверх как есть.
pub(super) struct InterruptRetrying<Inner> {
    inner: Inner,
}

impl<Inner> InterruptRetrying<Inner> {
    pub(super) fn new(inner: Inner) -> Self {
        Self { inner }
    }
}

impl<Inner: Read> Read for InterruptRetrying<Inner> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        retry_while_interrupted(|| self.inner.read(buffer))
    }
}

impl<Inner: Write> Write for InterruptRetrying<Inner> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        retry_while_interrupted(|| self.inner.write(bytes))
    }

    fn flush(&mut self) -> io::Result<()> {
        retry_while_interrupted(|| self.inner.flush())
    }
}

/// Зовёт `call`, пока он кончается `Interrupted`. Бесконечный повтор безопасен:
/// каждый вызов сам ограничен тайм-аутом сокета, а сигналы — разовые события
/// (заморозка перед сном, остановка отладчиком), а не постоянный поток.
fn retry_while_interrupted<Value>(
    mut call: impl FnMut() -> io::Result<Value>,
) -> io::Result<Value> {
    loop {
        match call() {
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            outcome => return outcome,
        }
    }
}

#[cfg(test)]
mod tests;
