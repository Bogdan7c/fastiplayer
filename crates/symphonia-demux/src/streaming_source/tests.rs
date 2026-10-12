use std::io::{ErrorKind, Read};

use bytes::Bytes;
use media_core::{DemuxReadEvent, Demuxer};

use super::StreamingByteReader;
use crate::SymphoniaDemuxer;

#[test]
fn chunked_stream_reaches_tracks_and_packet_before_clean_eof() {
    let wav = crate::factory::tests::generated_pcm_wav();
    let (writer, reader) = StreamingByteReader::channel();

    writer
        .send_chunk(Bytes::new())
        .expect("The empty network chunk should be a secure no-op");
    for chunk in wav.chunks(13) {
        writer
            .send_chunk(Bytes::copy_from_slice(chunk))
            .expect("The bounded stream should accept a small WAV fixture.");
    }
    writer.finish().expect("producer must be completed stream");

    let mut demuxer = SymphoniaDemuxer::from_stream(reader, "wav", "generated-stream.wav")
        .expect("chunked WAV must open through production streaming demux boundary");
    assert_eq!(demuxer.tracks().len(), 1);
    assert!(
        matches!(demuxer.next_event(), Ok(DemuxReadEvent::Packet(_))),
        "stream should reach the real media packet, not just the probe"
    );
    assert!(
        matches!(demuxer.next_event(), Ok(DemuxReadEvent::EndOfStream)),
        "Explicit producer EOF should become a full-time demux EOF"
    );
}

#[test]
fn producer_failure_reaches_the_stream_consumer_without_becoming_eof() {
    let (writer, mut reader) = StreamingByteReader::channel();
    writer
        .fail("fixture upstream aborted")
        .expect("Active reader must accept producer failure");

    let error = reader
        .read(&mut [0_u8; 8])
        .expect_err("producer failure You can't disguise yourself as a staff member EOF");
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.to_string().contains("fixture upstream aborted"));
}

#[test]
fn producer_failure_prevents_demux_publication() {
    let (writer, reader) = StreamingByteReader::channel();
    writer
        .fail("fixture upstream aborted")
        .expect("Active reader must accept producer failure");

    let Err(error) = SymphoniaDemuxer::from_stream(reader, "wav", "failed-stream.wav") else {
        panic!("demuxer You can't publish after upstream producer failure");
    };
    // Отказ несёт настоящую причину — сбой producer-а, а не «формат не найден»:
    // Symphonia probe сама превращает любую ошибку чтения в unsupported format.
    assert!(
        matches!(&error, crate::DemuxError::Io(_)),
        "The source failure must remain I/O-error: {error:?}"
    );
    assert!(
        error.to_string().contains("fixture upstream aborted"),
        "{error}"
    );
}

/// Обратная сторона: поток без ошибок, но с нераспознаваемыми байтами по-прежнему
/// отклоняется как неподдерживаемый формат (наблюдатель не выдумывает I/O-причину).
#[test]
fn clean_stream_with_unknown_bytes_stays_unsupported_format() {
    let (writer, reader) = StreamingByteReader::channel();
    writer
        .send_chunk(Bytes::from_static(b"definitely not a media container"))
        .expect("The bounded stream should accept the little chunk");
    writer.finish().expect("producer must be completed stream");

    let Err(error) = SymphoniaDemuxer::from_stream(reader, "wav", "unknown-stream.wav") else {
        panic!("Unrecognisable bytes should not be opened as media.");
    };

    assert!(
        matches!(error, crate::DemuxError::UnsupportedFormat(_)),
        "{error:?}"
    );
}
