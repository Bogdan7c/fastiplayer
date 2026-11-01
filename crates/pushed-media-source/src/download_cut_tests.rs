//! Отсечка обрыва скачивания на настоящих полосах и демуксерах: что плеер получит
//! после `end_download_cut_short`.
//!
//! Фикстуры: `video-h264-64-4s.mp4` (10 фрагментов по 0,4 с, кадры по 40 мс) и
//! `audio-aac-4s.mp4` (10 фрагментов по 405,33 мс, AAC-кадры по 21,33 мс), TS —
//! `muxed-h264-aac.ts` (0,4 с видео + звук в одной полосе).

use std::thread;
use std::time::{Duration, Instant};

use media_core::{DemuxDataEnd, DemuxReadEvent, DemuxSeekRequest, Demuxer, TrackKind};

use crate::container::PushedContainer;
use crate::download_cut::{DownloadCutOutcome, PlayableEnd};
use crate::lane::LaneId;
use crate::policy::PushedSourcePolicy;
use crate::stream::{OpenProgress, PushedStreamDemuxer, PushedStreamOpener, pushed_stream};
use crate::test_fixtures::{AUDIO_AAC_4S, SplitMp4, VIDEO_H264_64_4S, split_mp4};

const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);
/// Сколько ждать фоновых демуксеров.
const WAIT: Duration = Duration::from_secs(5);
/// Длительность фрагмента видео фикстуры.
const VIDEO_FRAGMENT: Duration = Duration::from_millis(400);
/// Конец звука фикстуры: 10 × 19 × 1024 sample при 48 кГц (последний пакет
/// начинается на пару AAC-кадров раньше).
const AUDIO_END: Duration = Duration::from_micros(4_053_333);
/// TS: видео и звук в одной полосе, без меток времени коробок.
const MUXED_TS: &[u8] = include_bytes!("../fixtures/muxed-h264-aac.ts");

/// Пакеты, которые получил плеер, по видам (время показа).
#[derive(Debug, Default)]
struct Delivered {
    video: Vec<Duration>,
    audio: Vec<Duration>,
}

impl Delivered {
    /// Самое позднее время среди пакетов вида.
    fn last(times: &[Duration]) -> Duration {
        times.iter().copied().max().expect("some packets")
    }
}

/// Поток видео + звук: видео — фрагменты `video_fragments`, звук — весь (4,05 с).
fn video_and_audio_stream(
    video_fragments: &[usize],
) -> (crate::stream::PushedStreamWriter, PushedStreamOpener) {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    let video: SplitMp4 = split_mp4(VIDEO_H264_64_4S);
    let audio: SplitMp4 = split_mp4(AUDIO_AAC_4S);
    writer.append(VIDEO, &video.init).expect("video init");
    for &index in video_fragments {
        writer
            .append(VIDEO, &video.fragments[index])
            .expect("video fragment");
    }
    writer.append(AUDIO, &audio.init).expect("audio init");
    for fragment in &audio.fragments {
        writer.append(AUDIO, fragment).expect("audio fragment");
    }
    (writer, opener)
}

fn open(opener: &mut PushedStreamOpener) -> PushedStreamDemuxer {
    let deadline = Instant::now() + WAIT;
    loop {
        match opener.poll_open().expect("opens") {
            OpenProgress::Ready(opened) => return opened.demuxer,
            OpenProgress::Waiting(hint) => {
                assert!(Instant::now() < deadline, "stream did not open");
                thread::sleep(hint.retry_after());
            }
        }
    }
}

/// Читает всё до конца потока.
fn read_to_end(demuxer: &mut PushedStreamDemuxer) -> Delivered {
    let deadline = Instant::now() + WAIT;
    let mut delivered = Delivered::default();
    loop {
        assert!(
            Instant::now() < deadline,
            "stream did not end: {delivered:?}"
        );
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::Packet(packet) => match packet.kind {
                TrackKind::Video => delivered.video.push(packet.pts),
                _ => delivered.audio.push(packet.pts),
            },
            DemuxReadEvent::EndOfStream => return delivered,
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            _ => {}
        }
    }
}

/// Отсечка из отчёта обрыва (тест падает, если обрыва не было).
fn cut_short_end(outcome: DownloadCutOutcome) -> PlayableEnd {
    match outcome {
        DownloadCutOutcome::CutShort(report) => report.playable_end,
        DownloadCutOutcome::AlreadyComplete => panic!("the download was not cut short"),
    }
}

/// Видео скачано до 1,2 с, звук до 4,05 с: отсечка — конец видео, звук за ней
/// плееру не уходит, а сам поток говорит «данные обрезаны».
#[test]
fn cut_ends_every_lane_at_the_earliest_lane_end() {
    let (mut writer, mut opener) = video_and_audio_stream(&[0, 1, 2]);
    let mut demuxer = open(&mut opener);
    assert_eq!(demuxer.data_end(), DemuxDataEnd::Complete);

    let outcome = writer.end_download_cut_short().expect("cut");

    let video_end = VIDEO_FRAGMENT * 3;
    assert_eq!(cut_short_end(outcome), PlayableEnd::At(video_end));
    let delivered = read_to_end(&mut demuxer);
    assert_eq!(delivered.video.len(), 30, "every downloaded frame plays");
    assert!(
        delivered.audio.iter().all(|&pts| pts < video_end),
        "no audio past the end of the video: {:?}",
        Delivered::last(&delivered.audio)
    );
    assert!(
        Delivered::last(&delivered.audio) >= video_end - Duration::from_millis(30),
        "audio plays right up to the cut: {:?}",
        Delivered::last(&delivered.audio)
    );
    assert_eq!(
        demuxer.data_end(),
        DemuxDataEnd::CutShort {
            playable_end: Some(video_end)
        }
    );
}

/// Дыра посреди видео (нет фрагмента 0,8–1,2 с, дальше видео есть до 4 с): играть
/// подряд можно только до дыры — там и отсечка, хотя за дырой данные есть.
#[test]
fn a_hole_in_one_lane_puts_the_cut_at_the_hole() {
    let (mut writer, mut opener) = video_and_audio_stream(&[0, 1, 3, 4, 5, 6, 7, 8, 9]);
    let mut demuxer = open(&mut opener);

    let outcome = writer.end_download_cut_short().expect("cut");

    let hole_start = VIDEO_FRAGMENT * 2;
    assert_eq!(cut_short_end(outcome), PlayableEnd::At(hole_start));
    let delivered = read_to_end(&mut demuxer);
    assert!(delivered.video.iter().all(|&pts| pts < hole_start));
    assert!(delivered.audio.iter().all(|&pts| pts < hole_start));
    assert_eq!(delivered.video.len(), 20);
    assert_eq!(
        demuxer.data_end(),
        DemuxDataEnd::CutShort {
            playable_end: Some(hole_start)
        }
    );
}

/// Перемотка во время досмотра за дыру: отсечка считается от новой позиции — звук
/// идёт до конца куска у цели, а не обрывается старой отсечкой у дыры.
#[test]
fn a_seek_while_cut_short_moves_the_cut_to_the_run_at_the_target() {
    let (mut writer, mut opener) = video_and_audio_stream(&[0, 1, 2, 4, 5, 6, 7, 8, 9]);
    let mut demuxer = open(&mut opener);
    let first_cut = cut_short_end(writer.end_download_cut_short().expect("cut"));
    assert_eq!(first_cut, PlayableEnd::At(VIDEO_FRAGMENT * 3));

    let target = Duration::from_secs(2);
    demuxer
        .seek_with_request(DemuxSeekRequest::accurate(target))
        .expect("seek within the downloaded media");

    let video_end = VIDEO_FRAGMENT * 10;
    let delivered = read_to_end(&mut demuxer);
    assert!(
        Delivered::last(&delivered.audio) > VIDEO_FRAGMENT * 3,
        "the old cut no longer applies after the seek"
    );
    assert!(
        delivered.audio.iter().all(|&pts| pts < video_end),
        "audio still stops at the end of the video at the target: {:?}",
        Delivered::last(&delivered.audio)
    );
    assert!(Delivered::last(&delivered.video) >= video_end - Duration::from_millis(40));
    assert_eq!(
        demuxer.data_end(),
        DemuxDataEnd::CutShort {
            playable_end: Some(video_end)
        }
    );
}

/// Вход уже закончен честно: обрывать нечего — поток доигрывается целиком, а
/// конец данных остаётся настоящим.
#[test]
fn a_cut_after_an_honest_end_of_stream_changes_nothing() {
    let (mut writer, mut opener) = video_and_audio_stream(&[0, 1, 2]);
    let mut demuxer = open(&mut opener);
    writer.end_of_stream().expect("honest end");

    assert_eq!(
        writer.end_download_cut_short().expect("no-op"),
        DownloadCutOutcome::AlreadyComplete
    );

    let delivered = read_to_end(&mut demuxer);
    assert!(
        Delivered::last(&delivered.audio) >= AUDIO_END - Duration::from_millis(50),
        "all downloaded audio plays: {:?}",
        Delivered::last(&delivered.audio)
    );
    assert_eq!(demuxer.data_end(), DemuxDataEnd::Complete);
}

/// Байты после обрыва снова открывают вход: обрыв снят, конец данных снова
/// настоящий (так MSE открывает поток после `endOfStream`).
#[test]
fn bytes_after_a_cut_resume_the_download() {
    let (mut writer, mut opener) = video_and_audio_stream(&[0, 1, 2]);
    let demuxer = open(&mut opener);
    writer.end_download_cut_short().expect("cut");
    assert_eq!(
        demuxer.data_end(),
        DemuxDataEnd::CutShort {
            playable_end: Some(VIDEO_FRAGMENT * 3)
        }
    );

    let video = split_mp4(VIDEO_H264_64_4S);
    writer
        .append(VIDEO, &video.fragments[3])
        .expect("bytes reopen the input");

    assert_eq!(demuxer.data_end(), DemuxDataEnd::Complete);
}

/// Полоса MPEG-TS времени коробок не знает: отсечки нет, всё полученное играется,
/// но плеер всё равно узнаёт, что данные обрезаны.
#[test]
fn a_ts_lane_plays_everything_without_a_cut() {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::MpegTs)
        .expect("ts lane");
    writer.append(VIDEO, MUXED_TS).expect("ts bytes");

    // Демуксер TS открывается, когда разберёт поток целиком или вход кончится:
    // здесь вход кончает сам обрыв.
    let outcome = writer.end_download_cut_short().expect("cut");
    let mut demuxer = open(&mut opener);

    assert_eq!(cut_short_end(outcome), PlayableEnd::Unknown);
    let delivered = read_to_end(&mut demuxer);
    assert_eq!(
        delivered.video.len(),
        10,
        "every TS frame plays: {delivered:?}"
    );
    assert!(!delivered.audio.is_empty());
    assert_eq!(
        demuxer.data_end(),
        DemuxDataEnd::CutShort { playable_end: None }
    );
}
