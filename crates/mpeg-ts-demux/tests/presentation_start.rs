//! Старт шкалы контейнера (`MediaContainerMetadata::presentation_start`).
//!
//! ffmpeg-запись начинается не с нуля (~1,4 с). Плеер показывает сырое время,
//! а внешние субтитры считают от нуля, поэтому demuxer обязан опубликовать старт.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test helpers fail the test by panicking"
)]

use std::collections::VecDeque;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use demux_api::{
    DemuxInput, OrderedSegment, OrderedSegmentDiscontinuity, OrderedSegmentKind,
    OrderedSegmentReadError, OrderedSegmentSequence, OrderedSegmentSource,
};
use media_core::{DemuxSeekRequest, Demuxer};
use mpeg_ts_demux::{MpegTsDemuxOptions, MpegTsDemuxer};
use source_core::{CancellationToken, LocalFileSource};

/// Первый PTS аудио в фикстуре (muxer ffmpeg сдвигает старт на 1,4 с).
const FIXTURE_START: Duration = Duration::from_millis(1_400);

fn part_a() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/concatenated/part-a-h264-ac3-440hz.ts");
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn part_b() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/concatenated/part-b-h264-ac3-700hz.ts");
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn start_of(demuxer: &MpegTsDemuxer) -> Option<Duration> {
    demuxer
        .media_metadata()
        .and_then(|metadata| metadata.container)
        .and_then(|container| container.presentation_start)
}

fn open_local_file(bytes: &[u8]) -> (tempfile::TempDir, MpegTsDemuxer) {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("recording.ts");
    std::fs::File::create(&path)
        .and_then(|mut file| file.write_all(bytes))
        .expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open local file");
    let demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open local file");
    (directory, demuxer)
}

#[test]
fn local_file_publishes_container_start_and_keeps_it_after_seek() {
    // Склейка двух записей: seek во вторую (эпоха не `StreamStart`) не должен
    // менять старт, принадлежащий первой записи.
    let bytes = [part_a(), part_b()].concat();
    let (_directory, mut demuxer) = open_local_file(&bytes);
    assert_eq!(start_of(&demuxer), Some(FIXTURE_START));

    demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(2_500),
        ))
        .expect("seek");
    assert_eq!(start_of(&demuxer), Some(FIXTURE_START));
}

#[test]
fn byte_stream_publishes_container_start_after_opening() {
    let demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_stream(Box::new(Cursor::new(part_a()))),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open byte stream");
    assert_eq!(start_of(&demuxer), Some(FIXTURE_START));
}

struct SingleSegment(VecDeque<OrderedSegment>);

impl OrderedSegmentSource for SingleSegment {
    fn next_segment(
        &mut self,
        _cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        Ok(self.0.pop_front())
    }
}

/// HLS-сегменты: шкалой владеет transport, demuxer старт не публикует.
#[test]
fn transport_owned_ordered_segments_publish_no_container_start() {
    let segment = OrderedSegment {
        sequence: OrderedSegmentSequence::new(0),
        kind: OrderedSegmentKind::Media,
        discontinuity: OrderedSegmentDiscontinuity::Continuous,
        bytes: Bytes::from(part_a()),
    };
    let demuxer = MpegTsDemuxer::open(
        DemuxInput::ordered_segments(Box::new(SingleSegment(VecDeque::from([segment])))),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open ordered segment");
    assert_eq!(start_of(&demuxer), None);
}
