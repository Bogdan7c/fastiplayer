//! Труба эпизода: целые сегменты от приёма → блокирующее чтение демуксера.
//!
//! Пишет поток приёма (соединение с браузером) или полка: они никогда не ждут.
//! Читает фоновый поток демуксера (`ProgressiveDemuxer`): он ждёт байтов, сколько
//! нужно, и плеер этого не чувствует — для плеера ожидание выглядит как
//! `TemporarilyUnavailable`.
//!
//! Труба кончается двумя путями, и они различаются:
//! - пишущий сказал «больше не будет» (новый init, смена кодека, конец потока) —
//!   читатель дочитывает остаток и получает чистый EOF;
//! - эпизод выброшен (плеер закрыл поток) — читатель сразу получает ошибку,
//!   а неизрасходованные сегменты отпускаются.
//!
//! Учёт байтов труба не ведёт сама: каждый сегмент — [`ChargedSegment`], уже
//! зачтённый при приёме. Дочитанный или выброшенный сегмент труба отпускает, и
//! учёт освобождается, когда отпустила последняя копия (полка может держать ту же).

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::intake::ChargedSegment;

/// Создаёт пустую трубу.
pub(crate) fn episode_pipe() -> (EpisodePipeWriter, EpisodePipeReader) {
    let shared = Arc::new(PipeShared {
        state: Mutex::new(PipeState {
            queued: VecDeque::new(),
            input_finished: false,
            abandoned: false,
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

/// Сегмент в очереди и сколько его байтов уже прочитано.
#[derive(Debug)]
struct QueuedSegment {
    /// Сегмент (общая с полкой память).
    segment: ChargedSegment,
    /// Прочитано байтов с начала сегмента.
    consumed: usize,
}

/// Очередь сегментов и флаги жизни трубы.
#[derive(Debug)]
struct PipeState {
    /// Ещё не дочитанные сегменты (первый может быть прочитан частично).
    queued: VecDeque<QueuedSegment>,
    /// Пишущий сказал «больше не будет».
    input_finished: bool,
    /// Эпизод выброшен: читать больше нечего и незачем.
    abandoned: bool,
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
    /// Ставит целый сегмент в очередь (байты уже зачтены при приёме).
    pub(crate) fn push(&self, segment: ChargedSegment) -> PipeWrite {
        let mut state = self.shared.lock();
        if state.abandoned {
            // Сегмент отпускается вне замка — вместе с `segment` в конце функции.
            return PipeWrite::EpisodeAbandoned;
        }
        state.queued.push_back(QueuedSegment {
            segment,
            consumed: 0,
        });
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
                let unread = front
                    .segment
                    .bytes()
                    .get(front.consumed..)
                    .unwrap_or_default();
                let copied = unread.len().min(buffer.len());
                buffer[..copied].copy_from_slice(&unread[..copied]);
                front.consumed += copied;
                if front.consumed >= front.segment.len() {
                    // Дочитанный сегмент труба отпускает: если полка его уже не
                    // держит, учёт освобождается здесь.
                    let finished = state.queued.pop_front();
                    drop(state);
                    drop(finished);
                }
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
    /// Выбрасывает трубу: читатель просыпается с ошибкой, сегменты отпускаются.
    pub(crate) fn abandon(&self) {
        let mut state = self.shared.lock();
        state.abandoned = true;
        let released = std::mem::take(&mut state.queued);
        drop(state);
        drop(released);
        self.shared.data_or_end.notify_all();
    }
}

#[cfg(test)]
#[path = "pipe_tests.rs"]
mod tests;
