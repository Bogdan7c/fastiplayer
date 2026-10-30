//! Тесты трубы эпизода: порядок байтов, EOF, выброс, учёт.

use std::io::Read;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use bytes::Bytes;

use super::{PipeWrite, episode_pipe};
use crate::intake::{ChargedSegment, IntakeLedger};

fn reserve(ledger: &Arc<IntakeLedger>, segment: &Bytes) -> ChargedSegment {
    ChargedSegment::charge(ledger, segment.clone()).expect("room")
}

#[test]
fn reader_gets_segments_in_order_then_clean_eof_and_ledger_drains() {
    let ledger = Arc::new(IntakeLedger::new(1024));
    let (writer, mut reader) = episode_pipe();
    let first = Bytes::from_static(b"init-");
    let second = Bytes::from_static(b"media");
    assert_eq!(writer.push(reserve(&ledger, &first)), PipeWrite::Queued);
    assert_eq!(writer.push(reserve(&ledger, &second)), PipeWrite::Queued);
    assert_eq!(ledger.held_bytes(), 10);
    writer.finish_input();

    let mut received = Vec::new();
    let mut small_buffer = [0_u8; 3];
    loop {
        let read = reader.read(&mut small_buffer).expect("readable");
        if read == 0 {
            break;
        }
        received.extend_from_slice(&small_buffer[..read]);
    }
    assert_eq!(received, b"init-media");
    assert_eq!(ledger.held_bytes(), 0);
}

#[test]
fn blocked_reader_wakes_on_new_bytes_and_on_dropped_writer() {
    let ledger = Arc::new(IntakeLedger::new(1024));
    let (writer, mut reader) = episode_pipe();
    let reading = thread::spawn(move || {
        let mut received = Vec::new();
        reader.read_to_end(&mut received).map(|_| received)
    });
    thread::sleep(Duration::from_millis(20));
    let segment = Bytes::from_static(b"late bytes");
    writer.push(reserve(&ledger, &segment));
    drop(writer);
    let received = reading.join().expect("reader thread").expect("clean EOF");
    assert_eq!(received, b"late bytes");
}

#[test]
fn abandon_wakes_reader_with_error_releases_bytes_and_refuses_new_segments() {
    let ledger = Arc::new(IntakeLedger::new(1024));
    let (writer, mut reader) = episode_pipe();
    let abandon = reader.abandon_handle();
    let queued = Bytes::from_static(b"never read");
    writer.push(reserve(&ledger, &queued));
    let reading = thread::spawn(move || {
        let mut sink = [0_u8; 64];
        // Первый read забирает очередь, второй ждёт и просыпается от выброса.
        let first = reader.read(&mut sink);
        let second = reader.read(&mut sink);
        (
            first.map_err(|error| error.kind()),
            second.map_err(|error| error.kind()),
        )
    });
    thread::sleep(Duration::from_millis(20));
    abandon.abandon();
    let (first, second) = reading.join().expect("reader thread");
    assert_eq!(first, Ok(10));
    assert_eq!(second, Err(std::io::ErrorKind::BrokenPipe));

    let refused = Bytes::from_static(b"too late");
    assert_eq!(
        writer.push(reserve(&ledger, &refused)),
        PipeWrite::EpisodeAbandoned
    );
    assert_eq!(ledger.held_bytes(), 0);
}

#[test]
fn segment_also_held_elsewhere_stays_charged_after_the_pipe_read_it() {
    let ledger = Arc::new(IntakeLedger::new(1024));
    let (writer, mut reader) = episode_pipe();
    let segment = reserve(&ledger, &Bytes::from_static(b"shelved"));
    // Полка держит ту же память, что и труба: учёт не удваивается.
    let shelf_copy = segment.clone();
    writer.push(segment);
    writer.finish_input();
    let mut received = Vec::new();
    reader.read_to_end(&mut received).expect("clean EOF");
    assert_eq!(received, b"shelved");
    assert_eq!(ledger.held_bytes(), 7);
    drop(shelf_copy);
    assert_eq!(ledger.held_bytes(), 0);
}

#[test]
fn dropping_both_sides_returns_unread_bytes_to_the_ledger() {
    let ledger = Arc::new(IntakeLedger::new(1024));
    let (writer, reader) = episode_pipe();
    let segment = Bytes::from_static(b"unread");
    writer.push(reserve(&ledger, &segment));
    assert_eq!(ledger.held_bytes(), 6);
    drop(writer);
    drop(reader);
    assert_eq!(ledger.held_bytes(), 0);
}
