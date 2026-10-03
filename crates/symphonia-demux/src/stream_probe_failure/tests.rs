use std::io::{self, Read};

use super::StreamProbeFailureReader;
use crate::DemuxError;

/// Reader, который отдаёт заранее заданную последовательность результатов `read`.
struct ScriptedReader {
    /// Результаты в порядке вызовов; после исчерпания — штатный EOF.
    script: Vec<io::Result<Vec<u8>>>,
}

impl ScriptedReader {
    fn new(script: Vec<io::Result<Vec<u8>>>) -> Self {
        Self { script }
    }
}

impl Read for ScriptedReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.script.is_empty() {
            return Ok(0);
        }
        let bytes = self.script.remove(0)?;
        output[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

/// Достаёт текст сохранённой ошибки, если наблюдатель вернул именно I/O-причину.
fn io_error_text(error: Option<DemuxError>) -> Option<String> {
    match error {
        Some(DemuxError::Io(io_error)) => Some(io_error.to_string()),
        Some(other) => panic!("observer must return only Io errors, got {other:?}"),
        None => None,
    }
}

/// Отсутствующий ресурс: источник читается без ошибок — причины для подмены нет.
#[test]
fn clean_stream_leaves_no_failure_to_report() {
    let (mut reader, observer) =
        StreamProbeFailureReader::new_observed(ScriptedReader::new(vec![Ok(b"data".to_vec())]));

    let mut buffer = [0_u8; 8];
    assert_eq!(reader.read(&mut buffer).expect("data read"), 4);
    assert_eq!(reader.read(&mut buffer).expect("clean EOF"), 0);

    assert!(observer.take_demux_error().is_none());
}

/// Ошибка во время probe сохраняется, а Symphonia получает копию того же вида и текста.
#[test]
fn probe_phase_failure_is_kept_and_mirrored_to_symphonia() {
    let (mut reader, observer) =
        StreamProbeFailureReader::new_observed(ScriptedReader::new(vec![Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "upstream reset",
        ))]));

    let mirrored = reader
        .read(&mut [0_u8; 8])
        .expect_err("read error must reach Symphonia");

    assert_eq!(mirrored.kind(), io::ErrorKind::ConnectionReset);
    assert_eq!(mirrored.to_string(), "upstream reset");
    assert_eq!(
        io_error_text(observer.take_demux_error()).as_deref(),
        Some("upstream reset")
    );
    // Причина отдаётся один раз: повторный take ничего не возвращает.
    assert!(observer.take_demux_error().is_none());
}

/// Accounting: хранится первая ошибка, а не последняя, и `Interrupted` не считается причиной.
#[test]
fn only_first_real_failure_is_kept() {
    let (mut reader, observer) = StreamProbeFailureReader::new_observed(ScriptedReader::new(vec![
        Err(io::Error::new(io::ErrorKind::Interrupted, "retry me")),
        Err(io::Error::other("first failure")),
        Err(io::Error::other("second failure")),
    ]));

    for _ in 0..3 {
        let _expected_error = reader.read(&mut [0_u8; 8]).expect_err("scripted error");
    }

    assert_eq!(
        io_error_text(observer.take_demux_error()).as_deref(),
        Some("first failure")
    );
}

/// После успешного probe наблюдатель не меняет runtime-ошибки и ничего не копит.
#[test]
fn runtime_errors_after_successful_probe_pass_through_unchanged() {
    let (mut reader, observer) =
        StreamProbeFailureReader::new_observed(ScriptedReader::new(vec![Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "runtime timeout",
        ))]));
    observer.finish_probe_success();

    let runtime_error = reader
        .read(&mut [0_u8; 8])
        .expect_err("runtime error must reach the caller");

    assert_eq!(runtime_error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(runtime_error.to_string(), "runtime timeout");
    assert!(observer.take_demux_error().is_none());
}
