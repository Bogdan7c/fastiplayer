//! Seek за пределы индекса: позиция ищется по смещению файла (бисекция по PTS или
//! пропорция для склеек), а не проходом по всему файлу. Окно индекса сделано
//! крошечным (1000 пакетов ≈ 166 кадров), чтобы цель лежала за много окон.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use media_core::DemuxSeekRequest;

use super::duration_probe::{
    FRAME_STEP_TICKS, RECORDING_START_TICKS, recording, ticks, window_options,
};
use super::*;
use crate::framing::TransportPacketReader;
use crate::seek_bisect::{
    BisectionOutcome, BisectionRequest, TimestampSample, bisect_to_timestamp,
};

/// Кадров в длинной записи: 10 минут, ~6.8 МБ.
const LONG_FRAMES: u64 = 6_000;
/// Окно индекса в пакетах: цель лежит за десятки окон.
const WINDOW_PACKETS: usize = 1_000;

/// Источник, считающий прочитанные байты: доказывает, что seek не читает файл целиком.
struct CountingSource {
    inner: LocalFileSource,
    bytes_read: Arc<AtomicU64>,
}

impl ByteSource for CountingSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        let count = self.inner.read(output, cancellation)?;
        self.bytes_read.fetch_add(count as u64, Ordering::SeqCst);
        Ok(count)
    }
    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.inner.seek(offset)
    }
    fn position(&self) -> u64 {
        self.inner.position()
    }
    fn seekability(&self) -> Seekability {
        self.inner.seekability()
    }
    fn validators(&self) -> SourceValidators {
        self.inner.validators()
    }
    fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }
    fn fingerprint(&self) -> SourceFingerprint {
        self.inner.fingerprint()
    }
}

fn open_counting(bytes: &[u8]) -> (tempfile::TempDir, MpegTsDemuxer, Arc<AtomicU64>) {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("far.ts");
    std::fs::write(&path, bytes).expect("write fixture");
    let bytes_read = Arc::new(AtomicU64::new(0));
    let source = CountingSource {
        inner: LocalFileSource::open(&path).expect("open source"),
        bytes_read: Arc::clone(&bytes_read),
    };
    let demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        window_options(WINDOW_PACKETS),
    )
    .expect("open TS");
    (directory, demuxer, bytes_read)
}

/// PTS кадра номер `index` (все кадры фикстуры — keyframe).
fn frame_pts(index: u64) -> Duration {
    ticks(RECORDING_START_TICKS + index * FRAME_STEP_TICKS)
}

/// Seek на середину кадра `index` и проверка: приземлились на сам кадр, а
/// воспроизведение продолжается с него без дыр.
fn assert_seek_lands_on_frame(demuxer: &mut MpegTsDemuxer, index: u64) {
    let target = frame_pts(index) + Duration::from_millis(30);
    let landed = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(target))
        .unwrap_or_else(|error| panic!("seek к кадру {index}: {error}"))
        .actual_position
        .as_duration();
    assert_eq!(landed, frame_pts(index), "кадр {index}");
    let mut played = Vec::new();
    while played.len() < 5 {
        match demuxer.next_event().expect("event") {
            DemuxReadEvent::Packet(packet) => played.push(packet.pts),
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
    }
    let expected: Vec<Duration> = (index..index + played.len() as u64)
        .map(frame_pts)
        .collect();
    assert_eq!(played, expected, "воспроизведение с кадра {index}");
}

/// Один seek на цель за десятки окон: точное приземление и ограниченный ввод-вывод.
#[test]
fn far_seek_lands_on_target_and_reads_a_fraction_of_the_file() {
    let bytes = recording(RECORDING_START_TICKS, LONG_FRAMES);
    let (_dir, mut demuxer, bytes_read) = open_counting(&bytes);
    let before_seek = bytes_read.load(Ordering::SeqCst);

    assert_seek_lands_on_frame(&mut demuxer, 3_000);

    // Читаются: одно окно расширения индекса, зонды бисекции и окно сканирования
    // цели — не файл. Воспроизведение после seek в счёт не входит (5 пакетов).
    let seek_bytes = bytes_read.load(Ordering::SeqCst) - before_seek;
    assert!(
        seek_bytes < bytes.len() as u64 / 4,
        "seek прочитал {seek_bytes} из {} байт",
        bytes.len()
    );
}

/// Серия далёких seek-ов вперёд и назад, в том числе обратно в покрытие индекса.
#[test]
fn repeated_far_seeks_forward_and_backward_land_exactly() {
    let bytes = recording(RECORDING_START_TICKS, LONG_FRAMES);
    let (_dir, mut demuxer, _) = open_counting(&bytes);

    for index in [3_000, 5_500, 1_200, 50, 4_000, 4_001, 0, 5_999] {
        assert_seek_lands_on_frame(&mut demuxer, index);
    }
}

/// `DecodePointBefore` сохраняет семантику: decode-safe точка не позже цели.
#[test]
fn far_seek_decode_point_before_keeps_its_semantics() {
    let bytes = recording(RECORDING_START_TICKS, LONG_FRAMES);
    let (_dir, mut demuxer, _) = open_counting(&bytes);
    let target = frame_pts(2_500) + Duration::from_millis(30);

    let landed = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("far decode-point seek")
        .actual_position
        .as_duration();

    assert_eq!(landed, frame_pts(2_500));
}

/// Склейка двух записей (PTS второй перезапущен): позиция приблизительная, но
/// время идёт непрерывно, а цель достигается с погрешностью меньше секунды.
#[test]
fn far_seek_in_concatenated_recordings_lands_near_the_target() {
    let part = recording(RECORDING_START_TICKS, 3_000);
    let bytes = [part.clone(), part].concat();
    let (_dir, mut demuxer, _) = open_counting(&bytes);
    let target = Duration::from_secs(450);

    let landed = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(target))
        .expect("seek in spliced file")
        .actual_position
        .as_duration();

    assert!(
        landed <= target && landed >= target - Duration::from_secs(1),
        "{landed:?}"
    );
    let mut played = Vec::new();
    while played.len() < 5 {
        if let DemuxReadEvent::Packet(packet) = demuxer.next_event().expect("event") {
            played.push(packet.pts);
        }
    }
    assert_eq!(played[0], landed);
    assert!(
        played
            .windows(2)
            .all(|pair| pair[1] - pair[0] == ticks(FRAME_STEP_TICKS))
    );
}

/// Цель далеко за концом: садимся на последний keyframe и играем его.
#[test]
fn far_seek_beyond_real_end_lands_on_last_keyframe() {
    let bytes = recording(RECORDING_START_TICKS, LONG_FRAMES);
    let (_dir, mut demuxer, _) = open_counting(&bytes);

    let landed = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(100_000)))
        .expect("seek past the end")
        .actual_position
        .as_duration();

    assert_eq!(landed, frame_pts(LONG_FRAMES - 1));
    assert_eq!(
        video_pts_after(&mut demuxer),
        vec![frame_pts(LONG_FRAMES - 1)]
    );
}

fn video_pts_after(demuxer: &mut MpegTsDemuxer) -> Vec<Duration> {
    super::duration_probe::video_pts(demuxer)
}

/// Завышенная оценка (хвост из null-пакетов без PTS): seek на «конец» оценки.
#[test]
fn far_seek_to_overestimated_duration_lands_on_last_keyframe() {
    let clean = recording(RECORDING_START_TICKS, LONG_FRAMES);
    let padding = TsFixtureBuilder::new().null_packets(3_000).finish();
    let (_dir, mut demuxer, _) = open_counting(&[clean, padding].concat());
    let estimate = demuxer.duration().expect("оценка по размеру");
    assert!(estimate > frame_pts(LONG_FRAMES - 1));

    let landed = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(estimate))
        .expect("seek to the estimated end")
        .actual_position
        .as_duration();

    // Завышенная оценка = склейка-подобный режим: позиция приблизительная, но не
    // дальше нескольких секунд (погрешность модели равномерного битрейта) от настоящего последнего keyframe.
    let last = frame_pts(LONG_FRAMES - 1);
    assert!(
        landed <= estimate && landed + Duration::from_secs(15) >= last,
        "{landed:?} vs {last:?}"
    );
}

/// PTS убывает с ростом смещения: бисекция обязана распознать немонотонность.
#[test]
fn bisection_detects_non_monotonic_timestamps() {
    let mut descending = Vec::new();
    for index in 0..600_u64 {
        descending.push(recording(RECORDING_START_TICKS + (600 - index) * 90_000, 1));
    }
    // Один PAT/PMT на запись допустим: parser их игнорирует после первого.
    let bytes = descending.concat();
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("descending.ts");
    std::fs::write(&path, &bytes).expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open source");
    let mut reader = TransportPacketReader::new(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    );
    let first_pts = (RECORDING_START_TICKS + 600 * 90_000) as i64;

    let outcome = bisect_to_timestamp(
        &mut reader,
        BisectionRequest {
            elementary_pids: &HashSet::from([VIDEO_PID]),
            aim_90khz: first_pts + 300 * 90_000,
            lower: TimestampSample {
                byte_offset: 0,
                pts_90khz: first_pts,
            },
            upper: TimestampSample {
                byte_offset: bytes.len() as u64,
                pts_90khz: first_pts + 600 * 90_000,
            },
            probe_bytes: 64 * 1024,
            bracket_bytes: 4 * 1024,
        },
    )
    .expect("bisection");

    assert_eq!(outcome, BisectionOutcome::NonMonotonic);
}
