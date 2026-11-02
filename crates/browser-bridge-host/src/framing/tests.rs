//! Тесты рамок: точное поведение на каждом виде входа.

use std::io::{self, Cursor, Read};

use super::*;

/// Рамка в формате native messaging.
fn frame(body: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(body.len()).unwrap().to_ne_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
}

/// Вход, который отдаёт не больше `chunk` байт за чтение и считает выданное.
struct ChunkedInput {
    bytes: Vec<u8>,
    position: usize,
    chunk: usize,
}

impl ChunkedInput {
    fn new(bytes: Vec<u8>, chunk: usize) -> Self {
        Self {
            bytes,
            position: 0,
            chunk,
        }
    }
}

impl Read for ChunkedInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = buffer
            .len()
            .min(self.chunk)
            .min(self.bytes.len() - self.position);
        buffer[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[test]
fn two_frames_in_one_buffer_are_read_one_by_one() {
    let mut input = frame(b"{\"a\":1}");
    input.extend(frame(b"{}"));
    let mut reader = FrameReader::new(Cursor::new(input));
    assert_eq!(
        reader.read_frame(),
        Ok(IncomingFrame::Message(b"{\"a\":1}".to_vec()))
    );
    assert_eq!(
        reader.read_frame(),
        Ok(IncomingFrame::Message(b"{}".to_vec()))
    );
    assert_eq!(reader.read_frame(), Ok(IncomingFrame::InputClosed));
}

#[test]
fn frames_cut_into_single_bytes_are_glued_back() {
    let mut input = frame(b"{\"type\":\"hello\"}");
    input.extend(frame(b"[1,2,3]"));
    let mut reader = FrameReader::new(ChunkedInput::new(input, 1));
    assert_eq!(
        reader.read_frame(),
        Ok(IncomingFrame::Message(b"{\"type\":\"hello\"}".to_vec()))
    );
    assert_eq!(
        reader.read_frame(),
        Ok(IncomingFrame::Message(b"[1,2,3]".to_vec()))
    );
    assert_eq!(reader.read_frame(), Ok(IncomingFrame::InputClosed));
}

#[test]
fn zero_length_frame_is_an_empty_message() {
    // Рамка цела; «пустой JSON» отвергает уже разбор протокола (отказ, порт жив).
    let mut reader = FrameReader::new(Cursor::new(frame(b"")));
    assert_eq!(reader.read_frame(), Ok(IncomingFrame::Message(Vec::new())));
    assert_eq!(reader.read_frame(), Ok(IncomingFrame::InputClosed));
}

#[test]
fn empty_input_is_a_closed_port() {
    let mut reader = FrameReader::new(Cursor::new(Vec::new()));
    assert_eq!(reader.read_frame(), Ok(IncomingFrame::InputClosed));
}

#[test]
fn input_ending_inside_the_prefix_is_a_truncated_length() {
    let mut reader = FrameReader::new(Cursor::new(vec![7, 0]));
    assert_eq!(
        reader.read_frame(),
        Err(FrameReadError::TruncatedLength { received: 2 })
    );
}

#[test]
fn input_ending_inside_the_body_is_a_truncated_message() {
    let mut input = frame(b"{\"type\":\"hello\"}");
    input.truncate(4 + 5);
    let mut reader = FrameReader::new(Cursor::new(input));
    assert_eq!(
        reader.read_frame(),
        Err(FrameReadError::TruncatedMessage {
            declared: 16,
            received: 5
        })
    );
}

#[test]
fn length_over_the_limit_is_rejected_without_reading_the_body() {
    let declared = u32::try_from(MAX_MESSAGE_FROM_BROWSER_BYTES + 1).unwrap();
    let mut input = declared.to_ne_bytes().to_vec();
    input.extend(vec![b'x'; 64]);
    let mut cursor = Cursor::new(input);
    let mut reader = FrameReader::new(&mut cursor);
    assert_eq!(
        reader.read_frame(),
        Err(FrameReadError::TooLarge {
            declared,
            limit: MAX_MESSAGE_FROM_BROWSER_BYTES
        })
    );
    // Прочитан только префикс: за объявленной длиной не идём и память не выделяем.
    assert_eq!(cursor.position(), 4);
}

#[test]
fn the_largest_declared_length_is_rejected_cheaply() {
    let mut cursor = Cursor::new(u32::MAX.to_ne_bytes().to_vec());
    let mut reader = FrameReader::new(&mut cursor);
    assert!(matches!(
        reader.read_frame(),
        Err(FrameReadError::TooLarge {
            declared: u32::MAX,
            ..
        })
    ));
    assert_eq!(cursor.position(), 4);
}

#[test]
fn a_message_of_exactly_the_limit_is_accepted() {
    let body = vec![b' '; MAX_MESSAGE_FROM_BROWSER_BYTES];
    let mut reader = FrameReader::new(Cursor::new(frame(&body)));
    assert_eq!(reader.read_frame(), Ok(IncomingFrame::Message(body)));
}

#[test]
fn the_reader_does_not_consume_bytes_of_the_next_frame() {
    let mut input = frame(b"{}");
    input.extend([0xAA, 0xBB]);
    let mut cursor = Cursor::new(input);
    let mut reader = FrameReader::new(&mut cursor);
    assert_eq!(
        reader.read_frame(),
        Ok(IncomingFrame::Message(b"{}".to_vec()))
    );
    assert_eq!(cursor.position(), 6);
}

/// Вход, который сначала прерывается сигналом, а потом отдаёт байты.
struct InterruptedOnce {
    interrupted: bool,
    inner: Cursor<Vec<u8>>,
}

impl Read for InterruptedOnce {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.interrupted {
            self.inner.read(buffer)
        } else {
            self.interrupted = true;
            Err(io::Error::from(io::ErrorKind::Interrupted))
        }
    }
}

#[test]
fn an_interrupted_read_is_retried() {
    let input = InterruptedOnce {
        interrupted: false,
        inner: Cursor::new(frame(b"{}")),
    };
    let mut reader = FrameReader::new(input);
    assert_eq!(
        reader.read_frame(),
        Ok(IncomingFrame::Message(b"{}".to_vec()))
    );
}

/// Вход, чтение которого всегда падает ошибкой ОС.
struct FailingInput;

impl Read for FailingInput {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

#[test]
fn an_os_read_error_is_reported_with_its_kind() {
    let mut reader = FrameReader::new(FailingInput);
    assert_eq!(
        reader.read_frame(),
        Err(FrameReadError::Read {
            kind: io::ErrorKind::PermissionDenied
        })
    );
}

#[test]
fn written_frame_has_native_length_prefix_and_exact_body() {
    let mut output = Vec::new();
    write_frame(&mut output, "{\"type\":\"refused\"}").unwrap();
    assert_eq!(output, frame(b"{\"type\":\"refused\"}"));
}

#[test]
fn reply_of_exactly_one_mebibyte_is_written() {
    let body = "x".repeat(MAX_MESSAGE_TO_BROWSER_BYTES);
    let mut output = Vec::new();
    write_frame(&mut output, &body).unwrap();
    assert_eq!(output.len(), 4 + MAX_MESSAGE_TO_BROWSER_BYTES);
}

#[test]
fn reply_over_one_mebibyte_is_not_written_at_all() {
    let body = "x".repeat(MAX_MESSAGE_TO_BROWSER_BYTES + 1);
    let mut output = Vec::new();
    assert_eq!(
        write_frame(&mut output, &body),
        Err(FrameWriteError::TooLarge {
            length: MAX_MESSAGE_TO_BROWSER_BYTES + 1,
            limit: MAX_MESSAGE_TO_BROWSER_BYTES
        })
    );
    // Браузер не должен получить даже префикс: иначе он ждал бы тело.
    assert!(output.is_empty());
}

/// Вывод, закрытый другой стороной.
struct ClosedOutput;

impl io::Write for ClosedOutput {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn writing_to_a_closed_browser_reports_broken_pipe() {
    assert_eq!(
        write_frame(&mut ClosedOutput, "{}"),
        Err(FrameWriteError::Write {
            kind: io::ErrorKind::BrokenPipe
        })
    );
}
