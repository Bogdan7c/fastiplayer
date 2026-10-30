//! Куда пишет журнал процесса: stderr, переживающий исчезнувшего читателя.
//!
//! Процесс браузерного режима запускается D-Bus-активацией, и его stderr — труба
//! активатора; обычный плеер, запущенный из терминала, пишет в терминал. Если читатель
//! исчез (выход из сеанса, упала шина, закрыли терминал, `| head`), запись в stderr
//! даёт `BrokenPipe` (Rust игнорирует SIGPIPE). `tracing-subscriber` сообщает об
//! ошибке записи через `eprintln!`, а тот на той же мёртвой трубе паникует; паника в
//! деструкторе во время раскрутки — `abort` (найдено в 03e по coredump-ам процесса
//! браузерного режима).
//!
//! Поэтому закрытая труба — не ошибка записи: читателя больше нет, сообщить о
//! потере строки некому и некуда, и процесс продолжает работу без журнала. Любая
//! другая ошибка записи возвращается как есть.

use std::io::{self, Write};

/// Писатель журнала для `tracing_subscriber::fmt().with_writer(...)`: stderr, у
/// которого закрытая труба считается записанной строкой.
pub(crate) fn closed_pipe_tolerant_stderr() -> ClosedPipeTolerant<io::Stderr> {
    ClosedPipeTolerant(io::stderr())
}

/// Обёртка писателя: `BrokenPipe` = «читателя нет, строка отброшена».
pub(crate) struct ClosedPipeTolerant<W>(W);

impl<W: Write> Write for ClosedPipeTolerant<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.0.write(buf) {
            // Читатель ушёл: строка честно «записана в никуда» — иначе `write_all`
            // повторял бы её, а слой журнала паниковал бы в `eprintln!`.
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(buf.len()),
            other => other,
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.0.flush() {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::ClosedPipeTolerant;

    /// Подставной писатель: отвечает заданной ошибкой или принимает байты.
    struct ScriptedWriter {
        failure: Option<io::ErrorKind>,
        written: Vec<u8>,
    }

    impl Write for ScriptedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match self.failure {
                Some(kind) => Err(io::Error::from(kind)),
                None => {
                    self.written.extend_from_slice(buf);
                    Ok(buf.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.failure
                .map_or(Ok(()), |kind| Err(io::Error::from(kind)))
        }
    }

    fn writer(failure: Option<io::ErrorKind>) -> ClosedPipeTolerant<ScriptedWriter> {
        ClosedPipeTolerant(ScriptedWriter {
            failure,
            written: Vec::new(),
        })
    }

    #[test]
    fn a_closed_pipe_drops_the_line_without_an_error() {
        let mut log = writer(Some(io::ErrorKind::BrokenPipe));
        assert!(log.write_all(b"INFO line\n").is_ok());
        assert!(log.flush().is_ok());
        assert!(log.0.written.is_empty());
    }

    #[test]
    fn a_live_reader_gets_every_byte() {
        let mut log = writer(None);
        log.write_all(b"INFO line\n").expect("written");
        assert_eq!(log.0.written, b"INFO line\n");
    }

    #[test]
    fn other_write_errors_are_still_reported() {
        let mut log = writer(Some(io::ErrorKind::PermissionDenied));
        let error = log
            .write_all(b"INFO line\n")
            .expect_err("not a closed pipe");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            log.flush().expect_err("flush fails too").kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
