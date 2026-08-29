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
        .expect("пустой network chunk должен быть безопасным no-op");
    for chunk in wav.chunks(13) {
        writer
            .send_chunk(Bytes::copy_from_slice(chunk))
            .expect("bounded stream должен принять маленький WAV fixture");
    }
    writer
        .finish()
        .expect("producer должен явно завершить stream");

    let mut demuxer = SymphoniaDemuxer::from_stream(reader, "wav", "generated-stream.wav")
        .expect("chunked WAV должен открыться через production streaming demux boundary");
    assert_eq!(demuxer.tracks().len(), 1);
    assert!(
        matches!(demuxer.next_event(), Ok(DemuxReadEvent::Packet(_))),
        "stream должен дойти до реального media packet, а не только до probe"
    );
    assert!(
        matches!(demuxer.next_event(), Ok(DemuxReadEvent::EndOfStream)),
        "явный producer EOF должен стать штатным demux EOF"
    );
}

#[test]
fn producer_failure_reaches_the_stream_consumer_without_becoming_eof() {
    let (writer, mut reader) = StreamingByteReader::channel();
    writer
        .fail("fixture upstream aborted")
        .expect("активный reader должен принять producer failure");

    let error = reader
        .read(&mut [0_u8; 8])
        .expect_err("producer failure нельзя маскировать как штатный EOF");
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.to_string().contains("fixture upstream aborted"));
}

#[test]
fn producer_failure_prevents_demux_publication() {
    let (writer, reader) = StreamingByteReader::channel();
    writer
        .fail("fixture upstream aborted")
        .expect("активный reader должен принять producer failure");

    assert!(
        SymphoniaDemuxer::from_stream(reader, "wav", "failed-stream.wav").is_err(),
        "demuxer нельзя публиковать после upstream producer failure"
    );
}
