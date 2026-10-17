//! Буферизованное чтение входа demux-а: seekable byte source или поток.
//!
//! Reader владеет буфером и абсолютными смещениями; что делать с байтами,
//! решает demuxer. Прочитанные байты не теряются между вызовами: незавершённый
//! хвост (половина кадра, ожидание dependent substream-а) остаётся в буфере.

use std::io::ErrorKind;

use ac3_syncframe::StreamContinuation;
use demux_api::{DemuxByteSource, DemuxByteStream, DemuxInput};
use source_core::{ByteSource, CancellationToken};

use crate::Ac3ElementaryDemuxError;

/// Конкретная форма входа.
enum InputReader {
    /// Local/HTTP byte source: seekability определяет сам source.
    Source(DemuxByteSource),
    /// Последовательный поток без seek-а.
    Stream(Box<dyn DemuxByteStream>),
}

/// Чем закончилось дочитывание.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadOutcome {
    /// В буфер добавлены новые байты.
    Appended,
    /// Вход кончился; буфер больше не вырастет.
    InputExhausted,
}

/// Буфер поверх входа с учётом абсолютных смещений.
pub(crate) struct ElementaryByteReader {
    input: InputReader,
    /// Прочитанные, но ещё не потреблённые (и уже потреблённые до компактации) байты.
    buffer: Vec<u8>,
    /// Сколько байт в начале `buffer` уже потреблено.
    consumed: usize,
    /// Абсолютное смещение `buffer[0]` во входе.
    buffer_start_offset: u64,
    /// Вход вернул EOF.
    input_exhausted: bool,
    cancellation: CancellationToken,
    read_chunk_bytes: usize,
}

impl ElementaryByteReader {
    /// Оборачивает вход; сегментированные входы сырому потоку не подходят.
    pub(crate) fn open(
        input: DemuxInput,
        cancellation: CancellationToken,
        read_chunk_bytes: usize,
    ) -> Result<Self, Ac3ElementaryDemuxError> {
        let (input, buffer_start_offset) = match input {
            DemuxInput::ByteSource(source) => {
                let position = source.position();
                (InputReader::Source(source), position)
            }
            DemuxInput::ByteStream(stream) => (InputReader::Stream(stream), 0),
            DemuxInput::OrderedSegments(_) => {
                return Err(Ac3ElementaryDemuxError::UnsupportedInput {
                    input: "ordered-segments",
                });
            }
            DemuxInput::OrderedResourceStream(_) => {
                return Err(Ac3ElementaryDemuxError::UnsupportedInput {
                    input: "ordered-resource-stream",
                });
            }
        };
        Ok(Self {
            input,
            buffer: Vec::new(),
            consumed: 0,
            buffer_start_offset,
            input_exhausted: false,
            cancellation,
            read_chunk_bytes,
        })
    }

    /// Непотреблённые байты.
    pub(crate) fn pending(&self) -> &[u8] {
        &self.buffer[self.consumed..]
    }

    /// Абсолютное смещение первого непотреблённого байта.
    pub(crate) fn pending_start_offset(&self) -> u64 {
        self.buffer_start_offset + self.consumed as u64
    }

    /// Отмечает `byte_count` байт с начала [`Self::pending`] потреблёнными.
    pub(crate) fn consume(&mut self, byte_count: usize) {
        self.consumed = self
            .consumed
            .saturating_add(byte_count)
            .min(self.buffer.len());
    }

    /// Может ли за буфером прийти ещё что-то.
    pub(crate) const fn continuation(&self) -> StreamContinuation {
        if self.input_exhausted {
            StreamContinuation::EndOfStream
        } else {
            StreamContinuation::MoreBytesMayFollow
        }
    }

    /// Вход дочитан до конца.
    pub(crate) const fn is_input_exhausted(&self) -> bool {
        self.input_exhausted
    }

    /// Поддерживает ли вход переход по байтам.
    pub(crate) fn is_seekable(&self) -> bool {
        matches!(&self.input, InputReader::Source(source) if source.seekability().is_seekable())
    }

    /// Размер входа, если известен.
    pub(crate) fn content_length(&self) -> Option<u64> {
        match &self.input {
            InputReader::Source(source) => source.content_length(),
            InputReader::Stream(_) => None,
        }
    }

    /// Дочитывает следующую порцию в конец буфера.
    pub(crate) fn read_more(&mut self) -> Result<ReadOutcome, Ac3ElementaryDemuxError> {
        if self.input_exhausted {
            return Ok(ReadOutcome::InputExhausted);
        }
        self.compact();
        let old_length = self.buffer.len();
        self.buffer.resize(old_length + self.read_chunk_bytes, 0);
        let read_result = self.read_into_tail(old_length);
        // Буфер всегда возвращается к фактически прочитанной длине, даже при ошибке.
        let appended = read_result.as_ref().map_or(0, |count| *count);
        self.buffer.truncate(old_length + appended);
        match read_result? {
            0 => {
                self.input_exhausted = true;
                Ok(ReadOutcome::InputExhausted)
            }
            _ => Ok(ReadOutcome::Appended),
        }
    }

    /// Дочитывает, пока непотреблённых байт меньше `minimum_bytes` и вход не кончился.
    pub(crate) fn fill_to(&mut self, minimum_bytes: usize) -> Result<(), Ac3ElementaryDemuxError> {
        while self.pending().len() < minimum_bytes {
            if self.read_more()? == ReadOutcome::InputExhausted {
                break;
            }
        }
        Ok(())
    }

    /// Пропускает `byte_count` байт входа: seek-ом, если можно, иначе чтением.
    pub(crate) fn skip(&mut self, byte_count: u64) -> Result<(), Ac3ElementaryDemuxError> {
        let target = self.pending_start_offset().saturating_add(byte_count);
        if self.is_seekable() {
            return self.reposition(target);
        }
        while self.pending_start_offset() < target {
            if self.pending().is_empty() && self.read_more()? == ReadOutcome::InputExhausted {
                return Ok(());
            }
            let remaining = target - self.pending_start_offset();
            let available = self.pending().len();
            self.consume(
                usize::try_from(remaining).map_or(available, |wanted| wanted.min(available)),
            );
        }
        Ok(())
    }

    /// Переставляет чтение на абсолютное смещение и сбрасывает буфер.
    pub(crate) fn reposition(&mut self, offset: u64) -> Result<(), Ac3ElementaryDemuxError> {
        let InputReader::Source(source) = &mut self.input else {
            return Err(Ac3ElementaryDemuxError::NotSeekable);
        };
        if !source.seekability().is_seekable() {
            return Err(Ac3ElementaryDemuxError::NotSeekable);
        }
        source
            .seek(offset)
            .map_err(Ac3ElementaryDemuxError::SourceRead)?;
        self.buffer.clear();
        self.consumed = 0;
        self.buffer_start_offset = offset;
        self.input_exhausted = false;
        Ok(())
    }

    /// Выбрасывает потреблённое начало буфера перед ростом.
    fn compact(&mut self) {
        if self.consumed > 0 {
            self.buffer.drain(..self.consumed);
            self.buffer_start_offset += self.consumed as u64;
            self.consumed = 0;
        }
    }

    /// Одно чтение в `buffer[start..]`; `Ok(0)` — конец входа.
    fn read_into_tail(&mut self, start: usize) -> Result<usize, Ac3ElementaryDemuxError> {
        if self.cancellation.is_cancelled() {
            return Err(Ac3ElementaryDemuxError::Cancelled);
        }
        let output = &mut self.buffer[start..];
        let result = match &mut self.input {
            InputReader::Source(source) => source
                .read(output, &self.cancellation)
                .map_err(Ac3ElementaryDemuxError::SourceRead),
            InputReader::Stream(stream) => {
                read_stream_retrying_interrupts(stream.as_mut(), output, &self.cancellation)
            }
        };
        // Ошибка чтения после отмены — это отмена, а не поломка источника.
        match result {
            Err(_) if self.cancellation.is_cancelled() => Err(Ac3ElementaryDemuxError::Cancelled),
            other => other,
        }
    }
}

/// `Read::read` с повтором `Interrupted`.
///
/// `Interrupted` бывает и от сигнала (повторяем), и от отмены: адаптеры
/// `source-core` переводят `Cancelled` именно в него. Поэтому перед повтором
/// проверяется token — иначе отменённое чтение крутилось бы вечно.
fn read_stream_retrying_interrupts(
    stream: &mut dyn DemuxByteStream,
    output: &mut [u8],
    cancellation: &CancellationToken,
) -> Result<usize, Ac3ElementaryDemuxError> {
    loop {
        match stream.read(output) {
            Ok(count) => return Ok(count),
            Err(error) if error.kind() == ErrorKind::Interrupted => {
                if cancellation.is_cancelled() {
                    return Err(Ac3ElementaryDemuxError::Cancelled);
                }
            }
            Err(error) => return Err(Ac3ElementaryDemuxError::StreamRead(error)),
        }
    }
}
