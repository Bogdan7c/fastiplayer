//! Обёртка повторяет только прерванные вызовы: данные приходят целиком и один раз,
//! остальные ошибки (`WouldBlock` шага опроса, обрыв) не глотаются и не повторяются.

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};

use super::InterruptRetrying;

/// Что ответит очередной вызов поддельного потока.
enum Step {
    /// Сигнал прервал вызов (`EINTR`).
    Interrupted,
    /// Вызов кончился другой ошибкой.
    Fails(ErrorKind),
    /// Чтение отдаёт эти байты / запись принимает столько байт / сброс удался.
    Succeeds(Vec<u8>),
}

/// Поток по сценарию: каждый `read`, `write` и `flush` берёт следующий шаг и считает вызовы.
#[derive(Default)]
struct ScriptedStream {
    steps: VecDeque<Step>,
    calls: usize,
    written: Vec<u8>,
}

impl ScriptedStream {
    fn with(steps: Vec<Step>) -> Self {
        Self {
            steps: steps.into(),
            ..Self::default()
        }
    }

    fn next_step(&mut self) -> Step {
        self.calls += 1;
        self.steps
            .pop_front()
            .unwrap_or(Step::Fails(ErrorKind::UnexpectedEof))
    }
}

impl Read for ScriptedStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.next_step() {
            Step::Interrupted => Err(ErrorKind::Interrupted.into()),
            Step::Fails(kind) => Err(kind.into()),
            Step::Succeeds(bytes) => {
                buffer[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
        }
    }
}

impl Write for ScriptedStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.next_step() {
            Step::Interrupted => Err(ErrorKind::Interrupted.into()),
            Step::Fails(kind) => Err(kind.into()),
            Step::Succeeds(accepted) => {
                // Поток «принимает» столько байт, сколько в шаге; записываем именно их.
                let taken = accepted.len().min(bytes.len());
                self.written.extend_from_slice(&bytes[..taken]);
                Ok(taken)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.next_step() {
            Step::Interrupted => Err(ErrorKind::Interrupted.into()),
            Step::Fails(kind) => Err(kind.into()),
            Step::Succeeds(_) => Ok(()),
        }
    }
}

#[test]
fn an_interrupted_read_is_repeated_until_the_bytes_arrive() {
    let mut stream = InterruptRetrying::new(ScriptedStream::with(vec![
        Step::Interrupted,
        Step::Interrupted,
        Step::Interrupted,
        Step::Succeeds(b"tick".to_vec()),
    ]));
    let mut buffer = [0_u8; 16];
    let read = stream.read(&mut buffer).unwrap();
    assert_eq!(&buffer[..read], b"tick");
    assert_eq!(
        stream.inner.calls, 4,
        "три прерывания и одно настоящее чтение"
    );
}

#[test]
fn an_empty_poll_step_after_an_interruption_still_reaches_the_caller() {
    // `WouldBlock` — это «за шаг опроса ничего не пришло»: решает цикл соединения,
    // а не обёртка. Она не должна ни глотать его, ни повторять.
    let mut stream = InterruptRetrying::new(ScriptedStream::with(vec![
        Step::Interrupted,
        Step::Fails(ErrorKind::WouldBlock),
        Step::Succeeds(b"late".to_vec()),
    ]));
    let mut buffer = [0_u8; 16];
    let error = stream.read(&mut buffer).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::WouldBlock);
    assert_eq!(stream.inner.calls, 2, "после WouldBlock вызовов больше нет");
}

#[test]
fn a_broken_link_is_reported_once_and_not_retried() {
    let mut stream = InterruptRetrying::new(ScriptedStream::with(vec![
        Step::Fails(ErrorKind::ConnectionReset),
        Step::Succeeds(b"never".to_vec()),
    ]));
    let mut buffer = [0_u8; 16];
    let error = stream.read(&mut buffer).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionReset);
    assert_eq!(stream.inner.calls, 1);
}

#[test]
fn an_interrupted_write_sends_its_bytes_exactly_once() {
    let mut stream = InterruptRetrying::new(ScriptedStream::with(vec![
        Step::Interrupted,
        Step::Interrupted,
        Step::Succeeds(b"clock".to_vec()),
    ]));
    let written = stream.write(b"clock").unwrap();
    assert_eq!(written, 5);
    assert_eq!(
        stream.inner.written, b"clock",
        "байты ушли один раз, без дублей"
    );
    assert_eq!(stream.inner.calls, 3);
}

#[test]
fn an_interrupted_flush_is_repeated_but_a_write_timeout_is_not() {
    let mut flushing = InterruptRetrying::new(ScriptedStream::with(vec![
        Step::Interrupted,
        Step::Succeeds(Vec::new()),
    ]));
    flushing.flush().unwrap();
    assert_eq!(flushing.inner.calls, 2);

    // Потолок записи (`write_timeout`) на Linux приходит как `WouldBlock`: это
    // решение «получатель завис», его принимает сервер, а не обёртка.
    let mut stuck = InterruptRetrying::new(ScriptedStream::with(vec![
        Step::Fails(ErrorKind::WouldBlock),
        Step::Succeeds(b"x".to_vec()),
    ]));
    let error = stuck.write(b"x").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::WouldBlock);
    assert_eq!(stuck.inner.calls, 1);
    assert!(stuck.inner.written.is_empty());
}
