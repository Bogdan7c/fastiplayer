//! Длительность больших seekable файлов, не влезших в начальный индекс.
//!
//! Раньше при `seek_scan_packets` < размера файла длительность оставалась `None`,
//! player-core не строил таймлайн и отказывал в seek-е. Теперь хвост файла даёт
//! точную длительность (одна запись) или оценку по размеру (склейка / мусор).

use std::time::Duration;

use media_core::{DemuxSeekRequest, DemuxSeekability};

use super::*;

/// Шаг видео: кадр каждые 0.1 с (все кадры — keyframe в этой фикстуре).
pub(super) const FRAME_STEP_TICKS: u64 = 9_000;
/// Старт PTS ffmpeg-записи эфира (1.4 с).
pub(super) const RECORDING_START_TICKS: u64 = 126_000;
/// Размер filler-а в AU: ~6 TS packets на кадр. Мелкий кадр нужен, чтобы окно
/// индекса покрывало десятки кадров: оценка по размеру ошибается примерно на
/// «один недописанный кадр головы», а при реальных ~6 МБ головы это тысячные доли.
const FILLER_BYTES: usize = 1_000;
/// Кадров в одной записи (≈ 60 с).
const FRAMES: u64 = 600;
/// Окно начального индекса: голова покрывает только первые ~165 кадров файла.
pub(super) const SMALL_SCAN_PACKETS: usize = 1_000;
/// Окно, которому хватает двух проходов (голова + один seek) на файл из ~7000 пакетов.
const TWO_PASS_SCAN_PACKETS: usize = 4_000;
/// Хвост не кратен 188 байтам: проверяет ресинхронизацию с произвольного смещения.
pub(super) const UNALIGNED_TAIL_BYTES: usize = 10_007;

/// Видео-only запись: `frames` keyframe-AU, PTS стартует с `first_pts`.
pub(super) fn recording(first_pts: u64, frames: u64) -> Vec<u8> {
    let mut access_unit = vec![
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00, 0x00, 0x01,
        0x65, 0x88,
    ];
    access_unit.extend(std::iter::repeat_n(0x11_u8, FILLER_BYTES));
    let mut builder =
        TsFixtureBuilder::new()
            .pat(&[(1, PMT_PID)], 0)
            .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0);
    for index in 0..frames {
        builder = builder.pes(
            VIDEO_PID,
            first_pts + index * FRAME_STEP_TICKS,
            None,
            &access_unit,
        );
    }
    builder.finish()
}

pub(super) fn ticks(value: u64) -> Duration {
    Duration::from_nanos(value * 1_000_000_000 / 90_000)
}

fn small_window_options() -> MpegTsDemuxOptions {
    window_options(SMALL_SCAN_PACKETS)
}

pub(super) fn window_options(scan_packets: usize) -> MpegTsDemuxOptions {
    MpegTsDemuxOptions {
        seek_scan_packets: MpegTsLimit::new(scan_packets, "seek_scan_packets").expect("limit"),
        tail_probe_bytes: MpegTsLimit::new(UNALIGNED_TAIL_BYTES, "tail_probe_bytes")
            .expect("limit"),
        ..MpegTsDemuxOptions::default()
    }
}

pub(super) fn open_file(
    bytes: &[u8],
    options: MpegTsDemuxOptions,
) -> (tempfile::TempDir, MpegTsDemuxer) {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("long.ts");
    std::fs::write(&path, bytes).expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open source");
    let demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        options,
    )
    .expect("open TS");
    (directory, demuxer)
}

/// PTS всех видеопакетов до EOS.
pub(super) fn video_pts(demuxer: &mut MpegTsDemuxer) -> Vec<Duration> {
    let mut pts = Vec::new();
    loop {
        match demuxer.next_event().expect("event") {
            DemuxReadEvent::Packet(packet) => pts.push(packet.pts),
            DemuxReadEvent::EndOfStream => return pts,
            _ => {}
        }
    }
}

fn relative_error(estimate: Duration, truth: Duration) -> f64 {
    (estimate.as_secs_f64() - truth.as_secs_f64()).abs() / truth.as_secs_f64()
}

/// Одна запись больше окна индекса: длительность известна при open и равна той,
/// что даёт полный проход до EOF (тот же максимум PTS).
#[test]
fn long_single_recording_has_exact_duration_at_open() {
    let bytes = recording(RECORDING_START_TICKS, FRAMES);
    let (_dir, demuxer) = open_file(&bytes, small_window_options());
    let (_dir_full, full_scan) = open_file(&bytes, MpegTsDemuxOptions::default());

    let last_pts = ticks(RECORDING_START_TICKS + (FRAMES - 1) * FRAME_STEP_TICKS);
    assert_eq!(
        full_scan.duration(),
        Some(last_pts),
        "эталон: EOF внутри окна"
    );
    assert_eq!(demuxer.duration(), Some(last_pts));
    assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
}

/// Чтение возвращено к началу: проба хвоста не съедает и не сдвигает playback.
#[test]
fn tail_probe_restores_playback_to_the_beginning() {
    let bytes = recording(RECORDING_START_TICKS, FRAMES);
    let (_dir, mut demuxer) = open_file(&bytes, small_window_options());
    let (_dir_full, mut reference) = open_file(&bytes, MpegTsDemuxOptions::default());

    let played = video_pts(&mut demuxer);

    assert_eq!(played.first(), Some(&ticks(RECORDING_START_TICKS)));
    assert_eq!(played, video_pts(&mut reference));
    assert_eq!(played.len(), FRAMES as usize);
}

/// 33-битный PTS переходит через 2^33 ближе к концу файла.
#[test]
fn pts_wrap_in_the_tail_gives_unwrapped_duration() {
    let first_pts = (1_u64 << 33) - 10 * 90_000;
    let bytes = recording(first_pts, FRAMES);
    let (_dir, demuxer) = open_file(&bytes, small_window_options());
    let (_dir_full, full_scan) = open_file(&bytes, MpegTsDemuxOptions::default());

    let expected = ticks(first_pts + (FRAMES - 1) * FRAME_STEP_TICKS);
    assert!(
        expected > ticks(1_u64 << 33),
        "хвост действительно за границей wrap"
    );
    assert_eq!(full_scan.duration(), Some(expected));
    assert_eq!(demuxer.duration(), Some(expected));
}

/// Склейка двух записей: PTS хвоста перезапущен. Длительность — оценка по размеру
/// (около суммы записей), а seek во вторую часть работает.
#[test]
fn concatenated_recordings_use_byte_rate_estimate_and_still_seek() {
    let part = recording(RECORDING_START_TICKS, FRAMES);
    let bytes = [part.clone(), part].concat();
    let (_dir, mut demuxer) = open_file(&bytes, window_options(TWO_PASS_SCAN_PACKETS));
    let (_dir_full, full_scan) = open_file(&bytes, MpegTsDemuxOptions::default());
    let truth = full_scan.duration().expect("полный проход видит весь файл");

    let estimate = demuxer.duration().expect("оценка по размеру");

    assert!(
        relative_error(estimate, truth) < 0.06,
        "{estimate:?} vs {truth:?}"
    );
    // Без перезапуска PTS хвост дал бы лишь ~61 с против ~121 с в сумме.
    assert!(estimate > Duration::from_secs(110));
    let landed = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(90)))
        .expect("seek во вторую запись (90 с)");
    let landed = landed.actual_position.as_duration();
    assert!(
        landed <= Duration::from_secs(90) && landed >= Duration::from_millis(89_800),
        "{landed:?}"
    );
    assert!(
        video_pts(&mut demuxer)
            .first()
            .is_some_and(|pts| *pts >= landed)
    );
}

/// Хвост — мусор без sync-байтов: проба не находит PTS, длительность считается по
/// размеру файла (мусор входит в размер, значит оценка чуть завышена, но не `None`).
#[test]
fn garbage_tail_falls_back_to_byte_rate_estimate() {
    let clean = recording(RECORDING_START_TICKS, FRAMES);
    let truth = ticks(RECORDING_START_TICKS + (FRAMES - 1) * FRAME_STEP_TICKS);
    let garbage_bytes = 60_000;
    let bytes = [clean.clone(), vec![0xaa; garbage_bytes]].concat();
    let (_dir, demuxer) = open_file(&bytes, small_window_options());

    let estimate = demuxer.duration().expect("оценка по размеру");

    let garbage_share = garbage_bytes as f64 / clean.len() as f64;
    assert!(
        estimate > truth,
        "мусор в размере завышает оценку: {estimate:?}"
    );
    assert!(
        relative_error(estimate, truth) < garbage_share + 0.06,
        "{estimate:?} vs {truth:?}"
    );
}

/// Seek за реальный конец при завышенной оценке: индекс дочитывается до EOF, seek
/// садится на последний keyframe, воспроизведение отдаёт его и завершается.
#[test]
fn seek_past_real_end_lands_on_last_keyframe() {
    let clean = recording(RECORDING_START_TICKS, FRAMES);
    let last = ticks(RECORDING_START_TICKS + (FRAMES - 1) * FRAME_STEP_TICKS);
    // Хвост из валидных null-пакетов без PTS: оценка по размеру завышена, но файл
    // остаётся корректным TS до EOF.
    let padding = TsFixtureBuilder::new().null_packets(500).finish();
    let bytes = [clean, padding].concat();
    let (_dir, mut demuxer) = open_file(&bytes, window_options(2_500));
    let estimate = demuxer.duration().expect("оценка по размеру");
    assert!(estimate > last, "оценка завышена");

    let result = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(
            estimate - Duration::from_millis(100),
        ))
        .expect("seek за конец");

    assert_eq!(result.actual_position.as_duration(), last);
    assert_eq!(video_pts(&mut demuxer), vec![last]);
}

/// Голова без диапазона PTS (скорость потока неизвестна): длительность остаётся `None`.
#[test]
fn head_without_pts_span_keeps_duration_unknown() {
    let bytes = recording(RECORDING_START_TICKS, FRAMES);
    let options = MpegTsDemuxOptions {
        seek_scan_packets: MpegTsLimit::new(8, "seek_scan_packets").expect("limit"),
        ..small_window_options()
    };
    let (_dir, demuxer) = open_file(&bytes, options);

    assert_eq!(demuxer.duration(), None);
}

/// Forward-only вход: пробы нет, поведение прежнее (длительность неизвестна).
#[test]
fn non_seekable_stream_does_not_probe_the_tail() {
    let bytes = recording(RECORDING_START_TICKS, FRAMES);
    let mut demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        CancellationToken::never_cancelled(),
        small_window_options(),
    )
    .expect("open stream");

    assert_eq!(demuxer.duration(), None);
    assert!(matches!(
        demuxer.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
    assert_eq!(video_pts(&mut demuxer).len(), FRAMES as usize);
}
