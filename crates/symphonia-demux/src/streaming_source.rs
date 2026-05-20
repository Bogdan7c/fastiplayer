//! Потоковый byte-source для demuxer-а.
//!
//! Сеть пишет `Bytes` чанки в bounded channel, а Symphonia читает их через обычный `Read`.
//! Такой слой отделяет HTTP backpressure от контейнерного demux-а.

use std::io::{self, Read};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use bytes::Bytes;

/// Сколько HTTP chunks можно держать между fetcher thread и demuxer thread.
const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// Сообщение, которое fetcher отправляет reader-у.
enum StreamChunk {
    /// Очередная порция bytes из сети.
    Data(Bytes),

    /// Сеть закончилась штатно.
    EndOfStream,

    /// Fetcher получил ошибку и demuxer должен остановиться.
    Error(String),
}

/// Writer-сторона потокового source.
#[derive(Clone)]
pub struct StreamingByteWriter {
    /// Bounded sender: если demuxer не успевает, HTTP fetcher блокируется.
    sender: SyncSender<StreamChunk>,
}

impl StreamingByteWriter {
    /// Отправляет следующий chunk bytes.
    pub fn send_chunk(&self, chunk: Bytes) -> io::Result<()> {
        // Пустые chunks не несут данных и только будят reader.
        if chunk.is_empty() {
            return Ok(());
        }

        // Send error означает, что reader уже закрыт; для fetcher это обычная остановка.
        self.sender
            .send(StreamChunk::Data(chunk))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "stream reader closed"))
    }

    /// Сообщает reader-у штатный EOF.
    pub fn finish(&self) -> io::Result<()> {
        // Если reader уже закрыт, дополнительный EOF не нужен.
        self.sender
            .send(StreamChunk::EndOfStream)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "stream reader closed"))
    }

    /// Сообщает reader-у ошибку fetcher-а.
    pub fn fail(&self, error: impl Into<String>) -> io::Result<()> {
        // Ошибка передаётся как строка, чтобы не тащить конкретный HTTP error type в demux crate.
        self.sender
            .send(StreamChunk::Error(error.into()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "stream reader closed"))
    }
}

/// Reader-сторона потокового source.
pub struct StreamingByteReader {
    /// Receiver лежит в Mutex, чтобы reader был `Sync` для `symphonia::ReadOnlySource`.
    receiver: Mutex<Receiver<StreamChunk>>,

    /// Текущий chunk, из которого ещё не всё скопировано в caller buffer.
    current_chunk: Bytes,

    /// Смещение чтения внутри `current_chunk`.
    current_offset: usize,

    /// Флаг EOF: после него `read()` должен возвращать 0.
    finished: bool,
}

impl StreamingByteReader {
    /// Создаёт bounded streaming channel.
    pub fn channel() -> (StreamingByteWriter, Self) {
        // sync_channel даёт естественный backpressure без async runtime.
        let (sender, receiver) = sync_channel(DEFAULT_CHANNEL_CAPACITY);

        let writer = StreamingByteWriter { sender };
        let reader = Self {
            receiver: Mutex::new(receiver),
            current_chunk: Bytes::new(),
            current_offset: 0,
            finished: false,
        };

        (writer, reader)
    }

    /// Гарантирует, что `current_chunk` содержит bytes или reader находится в EOF/error.
    fn refill_current_chunk_if_needed(&mut self) -> io::Result<()> {
        // Если в текущем chunk ещё есть bytes, новый chunk не нужен.
        if self.current_offset < self.current_chunk.len() {
            return Ok(());
        }

        // После EOF новые bytes уже не появятся.
        if self.finished {
            return Ok(());
        }

        // Ждём следующий network chunk. Это блокирующая точка demuxer-а.
        let next_chunk = self
            .receiver
            .lock()
            .map_err(|_| io::Error::other("stream receiver mutex poisoned"))?
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "stream writer closed"))?;

        match next_chunk {
            StreamChunk::Data(chunk) => {
                self.current_chunk = chunk;
                self.current_offset = 0;
            }
            StreamChunk::EndOfStream => {
                self.finished = true;
                self.current_chunk = Bytes::new();
                self.current_offset = 0;
            }
            StreamChunk::Error(error) => {
                return Err(io::Error::other(error));
            }
        }

        Ok(())
    }
}

impl Read for StreamingByteReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        // Zero-length read всегда успешен.
        if output.is_empty() {
            return Ok(0);
        }

        // Если данных нет, блокируемся до chunk/EOF/error.
        self.refill_current_chunk_if_needed()?;

        // EOF для Read выражается как Ok(0).
        if self.finished && self.current_offset >= self.current_chunk.len() {
            return Ok(0);
        }

        // Копируем столько, сколько помещается в caller buffer.
        let available_bytes = &self.current_chunk[self.current_offset..];
        let bytes_to_copy = available_bytes.len().min(output.len());
        output[..bytes_to_copy].copy_from_slice(&available_bytes[..bytes_to_copy]);
        self.current_offset += bytes_to_copy;

        Ok(bytes_to_copy)
    }
}
