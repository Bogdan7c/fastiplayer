//! Труба эпизода: целые сегменты от приёма → блокирующее чтение демуксера.
//!
//! Пишет поток приёма (соединение с браузером): он никогда не ждёт. Читает фоновый
//! поток демуксера (`ProgressiveDemuxer`): он ждёт байтов, сколько нужно, и плеер
//! этого не чувствует — для плеера ожидание выглядит как `TemporarilyUnavailable`.
//!
//! Труба кончается двумя путями, и они различаются:
//! - пишущий сказал «больше не будет» (новый init, смена кодека, конец потока) —
//!   читатель дочитывает остаток и получает чистый EOF;
//! - эпизод выброшен (плеер закрыл поток) — читатель сразу получает ошибку,
//!   а неизрасходованные байты возвращаются в общий учёт потока.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use bytes::{Buf, Bytes};

use crate::intake::IntakeLedger;

/// Создаёт пустую трубу, байты которой учитываются в общем учёте потока.
pub(crate) fn episode_pipe(ledger: Arc<IntakeLedger>) -> (EpisodePipeWriter, EpisodePipeReader) {
    let shared = Arc::new(PipeShared {
        state: Mutex::new(PipeState {
            queued: VecDeque::new(),
            queued_bytes: 0,
            input_finished: false,
            abandoned: false,
            ledger,
        }),
        data_or_end: Condvar::new(),
    });
    (
        EpisodePipeWriter {
            shared: Arc::clone(&shared),
        },
        EpisodePipeReader { shared },
    )
}

/// Общее состояние трубы.
#[derive(Debug)]
struct PipeShared {
    /// Очередь и флаги жизни.
    state: Mutex<PipeState>,
    /// Будит читателя: пришли байты, вход окончен или эпизод выброшен.
    data_or_end: Condvar,
}

/// Очередь сегментов и флаги жизни трубы.
#[derive(Debug)]
struct PipeState {
    /// Ещё не прочитанные сегменты (первый может быть прочитан частично).
    queued: VecDeque<Bytes>,
    /// Сколько байтов в `queued` — то же число лежит в общем учёте.
    queued_bytes: usize,
    /// Пишущий сказал «больше не будет».
    input_finished: bool,
    /// Эпизод выброшен: читать больше нечего и незачем.
    abandoned: bool,
    /// Общий учёт удерживаемых байтов потока.
    ledger: Arc<IntakeLedger>,
}

impl Drop for PipeState {
    /// Байты, которые так и не прочитали, перестают занимать общий учёт.
    fn drop(&mut self) {
        self.ledger.release(self.queued_bytes);
    }
}

impl PipeShared {
    /// Захватывает состояние; отравленный mutex не мешает освобождению ресурсов.
    fn lock(&self) -> MutexGuard<'_, PipeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Пишущая сторона: у текущего эпизода полосы.
#[derive(Debug)]
pub(crate) struct EpisodePipeWriter {
    /// Общее состояние.
    shared: Arc<PipeShared>,
}

/// Итог попытки записать сегмент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PipeWrite {
    /// Сегмент в очереди.
    Queued,
    /// Эпизод уже выброшен читателем: сегмент никому не нужен.
    EpisodeAbandoned,
}

impl EpisodePipeWriter {
    /// Ставит целый сегмент в очередь. Байты уже зачтены в общий учёт вызывающим.
    pub(crate) fn push_reserved(&self, segment: Bytes) -> PipeWrite {
        let mut state = self.shared.lock();
        if state.abandoned {
            state.ledger.release(segment.len());
            return PipeWrite::EpisodeAbandoned;
        }
        state.queued_bytes += segment.len();
        state.queued.push_back(segment);
        drop(state);
        self.shared.data_or_end.notify_all();
        PipeWrite::Queued
    }

    /// «Больше не будет»: читатель дочитает остаток и получит EOF.
    pub(crate) fn finish_input(&self) {
        self.shared.lock().input_finished = true;
        self.shared.data_or_end.notify_all();
    }
}

impl Drop for EpisodePipeWriter {
    /// Брошенный пишущий тоже означает конец входа — читатель не должен ждать вечно.
    fn drop(&mut self) {
        self.finish_input();
    }
}

/// Читающая сторона: отдаётся демуксеру эпизода как `Read`.
#[derive(Debug)]
pub(crate) struct EpisodePipeReader {
    /// Общее состояние.
    shared: Arc<PipeShared>,
}

impl EpisodePipeReader {
    /// Ручка, которой владелец эпизода выбрасывает трубу.
    pub(crate) fn abandon_handle(&self) -> EpisodePipeAbandon {
        EpisodePipeAbandon {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Read for EpisodePipeReader {
    /// Ждёт байты; EOF — только после «больше не будет» и пустой очереди.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut state = self.shared.lock();
        loop {
            if state.abandoned {
                // Не `Interrupted`: читатели (`read_exact` и демуксеры) считают его
                // «повтори», и выброшенный эпизод крутил бы поток вхолостую.
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the pushed media episode was abandoned",
                ));
            }
            if let Some(front) = state.queued.front_mut() {
                let copied = front.len().min(buffer.len());
                buffer[..copied].copy_from_slice(&front[..copied]);
                front.advance(copied);
                if front.is_empty() {
                    state.queued.pop_front();
                }
                state.queued_bytes -= copied;
                state.ledger.release(copied);
                return Ok(copied);
            }
            if state.input_finished {
                return Ok(0);
            }
            state = self
                .shared
                .data_or_end
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// Ручка выброса трубы: у владельца эпизода, срабатывает при его уничтожении.
#[derive(Debug)]
pub(crate) struct EpisodePipeAbandon {
    /// Общее состояние.
    shared: Arc<PipeShared>,
}

impl EpisodePipeAbandon {
    /// Выбрасывает трубу: читатель просыпается с ошибкой, байты уходят из учёта.
    pub(crate) fn abandon(&self) {
        let mut state = self.shared.lock();
        state.abandoned = true;
        state.ledger.release(state.queued_bytes);
        state.queued_bytes = 0;
        state.queued.clear();
        drop(state);
        self.shared.data_or_end.notify_all();
    }
}

#[cfg(test)]
#[path = "pipe_tests.rs"]
mod tests;
