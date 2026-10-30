//! Позднее вступление полосы после перемотки (правило 5 `stream_buffer::seek`):
//! полоса без данных у цели вступает со своего первого сегмента после цели, если
//! другие полосы непрерывно покрывают время от цели до него, — с настоящими
//! демуксерами видео и звука.
//!
//! Фикстуры: `video-h264-64-4s.mp4` (10 фрагментов по 0,4 с, каждый с ключевого,
//! кадры по 40 мс) и `audio-aac-4s.mp4` (фрагмент N начинается в N × 405,33 мс).

use std::thread;
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, MediaTime, TrackKind};

use super::{
    OpenProgress, PushedStreamDemuxer, PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use crate::container::PushedContainer;
use crate::framing::MediaTime as ShelfTime;
use crate::lane::LaneId;
use crate::monitor::PushedStreamMonitor;
use crate::page_seek::PageSeekRequest;
use crate::policy::PushedSourcePolicy;
use crate::stream_buffer::LaneLanding;
use crate::test_fixtures::{AUDIO_AAC_4S, SplitMp4, VIDEO_H264_64_4S, split_mp4};
use crate::time::PushedDuration;

const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);
/// Сколько ждать фоновых демуксеров.
const WAIT: Duration = Duration::from_secs(5);
/// Sample-ов звука во фрагменте (19 AAC-кадров по 1024) при 48 кГц.
const AUDIO_SAMPLES_PER_FRAGMENT: i64 = 19 * 1_024;

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// Начало фрагмента звука `index` на шкале полки (мкс, вниз, как разметка).
fn audio_fragment_start(index: i64) -> ShelfTime {
    ShelfTime::from_micros(index * AUDIO_SAMPLES_PER_FRAGMENT * 1_000_000 / 48_000)
}

/// Начало фрагмента звука в миллисекундах (как время пакетов в `read_to_end`).
fn audio_fragment_start_ms(index: i64) -> u64 {
    u64::try_from(audio_fragment_start(index).as_micros() / 1_000).expect("positive")
}

/// Поток «видео + звук» (длительность 4 с) с отправленными init-сегментами.
struct Stream {
    writer: PushedStreamWriter,
    opener: PushedStreamOpener,
    monitor: PushedStreamMonitor,
    video: SplitMp4,
    audio: SplitMp4,
}

impl Stream {
    fn new() -> Self {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let (mut writer, opener) = pushed_stream(policy).expect("stream");
        writer
            .add_lane(VIDEO, PushedContainer::FragmentedMp4)
            .expect("video lane");
        writer
            .add_lane(AUDIO, PushedContainer::FragmentedMp4)
            .expect("audio lane");
        writer.set_duration(PushedDuration::Finite(Duration::from_secs(4)));
        let video = split_mp4(VIDEO_H264_64_4S);
        let audio = split_mp4(AUDIO_AAC_4S);
        writer.append(VIDEO, &video.init).expect("video init");
        writer.append(AUDIO, &audio.init).expect("audio init");
        Self {
            monitor: writer.monitor(),
            writer,
            opener,
            video,
            audio,
        }
    }

    fn push(&mut self, lane: LaneId, fragments: std::ops::Range<usize>) {
        let media = if lane == VIDEO {
            &self.video
        } else {
            &self.audio
        };
        let bytes = media.fragments[fragments].concat();
        self.writer.append(lane, &bytes).expect("fragments");
    }

    fn open(&mut self) -> PushedStreamDemuxer {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.opener.poll_open().expect("opens") {
                OpenProgress::Ready(opened) => return opened.demuxer,
                OpenProgress::Waiting(hint) => {
                    assert!(Instant::now() < deadline, "stream did not open");
                    thread::sleep(hint.retry_after());
                }
            }
        }
    }
}

/// Куда последняя перемотка поставила полосу.
fn landing(demuxer: &PushedStreamDemuxer, lane: LaneId) -> Option<LaneLanding> {
    demuxer.buffer.lane_landing(lane)
}

/// Времена пакетов (мс) видео и звука до конца потока.
fn read_to_end(demuxer: &mut PushedStreamDemuxer) -> (Vec<u64>, Vec<u64>) {
    let deadline = Instant::now() + WAIT;
    let (mut video, mut audio) = (Vec::new(), Vec::new());
    loop {
        assert!(
            Instant::now() < deadline,
            "stream did not end: video {video:?}, audio {audio:?}"
        );
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::Packet(packet) => {
                let pts = u64::try_from(packet.pts.as_millis()).expect("small");
                match packet.kind {
                    TrackKind::Video => video.push(pts),
                    _ => audio.push(pts),
                }
            }
            DemuxReadEvent::EndOfStream => return (video, audio),
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            _ => {}
        }
    }
}

/// Кадры видео с `from_ms` до конца (по 40 мс).
fn video_frames_from(from_ms: u64) -> Vec<u64> {
    (from_ms / 40..100).map(|frame| frame * 40).collect()
}

#[test]
fn on_shelf_seek_into_the_audio_leading_gap_joins_audio_at_its_first_segment() {
    let mut stream = Stream::new();
    stream.push(VIDEO, 0..10);
    stream.push(AUDIO, 1..10);
    stream.writer.end_of_stream().expect("end");
    let mut demuxer = stream.open();

    let landed = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(ms(200)))
        .expect("seek");

    // Всё на полке: видео держит цель, звук вступает позже — страницу не просят.
    assert_eq!(stream.monitor.take_page_seek_request(), None);
    assert_eq!(
        landing(&demuxer, VIDEO),
        Some(LaneLanding::FromShelf(ShelfTime::from_micros(0)))
    );
    assert_eq!(
        landing(&demuxer, AUDIO),
        Some(LaneLanding::FromShelf(audio_fragment_start(1)))
    );
    // Ответ плееру не позже цели (видео — с начала фрагмента с целью).
    assert_eq!(landed.actual_position, MediaTime::from_duration(ms(0)));
    let (video, audio) = read_to_end(&mut demuxer);
    assert_eq!(video, video_frames_from(0));
    assert_eq!(audio.first().copied(), Some(audio_fragment_start_ms(1)));
    assert!(
        audio.windows(2).all(|pair| pair[0] < pair[1]),
        "audio in order: {audio:?}"
    );
}

#[test]
fn lane_holding_the_target_lands_on_its_own_segment_not_a_later_one() {
    // Контроль: у звука цель есть — он садится на сегмент с целью, как раньше.
    let mut stream = Stream::new();
    stream.push(VIDEO, 0..10);
    stream.push(AUDIO, 0..10);
    stream.writer.end_of_stream().expect("end");
    let mut demuxer = stream.open();

    demuxer.seek(ms(1_000)).expect("seek");

    assert_eq!(stream.monitor.take_page_seek_request(), None);
    assert_eq!(
        landing(&demuxer, AUDIO),
        Some(LaneLanding::FromShelf(audio_fragment_start(2)))
    );
    let (video, audio) = read_to_end(&mut demuxer);
    assert_eq!(video, video_frames_from(800));
    assert_eq!(audio.first().copied(), Some(audio_fragment_start_ms(2)));
}

#[test]
fn late_lane_joins_only_when_other_lanes_cover_time_up_to_its_first_segment() {
    let mut stream = Stream::new();
    stream.push(VIDEO, 0..3);
    stream.push(AUDIO, 0..3);
    let mut demuxer = stream.open();
    let target = ms(2_480);

    let landed = demuxer.seek(target).expect("seek");

    // Цели нет ни у одной полосы: одна просьба странице, полосы ждут.
    assert_eq!(landed.actual_position, MediaTime::from_duration(target));
    assert_eq!(
        stream.monitor.take_page_seek_request(),
        Some(PageSeekRequest { target })
    );
    let awaiting = Some(LaneLanding::AwaitingPage(ShelfTime::from_micros(2_480_000)));
    // Страница шлёт звук только со своего сегмента после цели (2,837 с) — раньше
    // видео. Опоры нет: звук ждёт (это может быть ещё не присланное время).
    stream.push(AUDIO, 7..10);
    assert_eq!(landing(&demuxer, AUDIO), awaiting);
    // Видео покрывает 2,4–2,8 с — раньше начала звука: вступать рано.
    stream.push(VIDEO, 6..7);
    assert_eq!(
        landing(&demuxer, VIDEO),
        Some(LaneLanding::FromShelf(ShelfTime::from_micros(2_400_000)))
    );
    assert_eq!(landing(&demuxer, AUDIO), awaiting);
    // Видео покрыло начало звука: звук вступает со своего первого сегмента.
    stream.push(VIDEO, 7..10);
    assert_eq!(
        landing(&demuxer, AUDIO),
        Some(LaneLanding::FromShelf(audio_fragment_start(7)))
    );
    stream.writer.end_of_stream().expect("end");

    let (video, audio) = read_to_end(&mut demuxer);
    assert_eq!(video, video_frames_from(2_400));
    // Ни пакета старого времени (0–1,2 с) и ни одного до начала звука страницы.
    assert_eq!(audio.first().copied(), Some(audio_fragment_start_ms(7)));
    assert_eq!(
        stream.monitor.take_page_seek_request(),
        None,
        "one request per seek"
    );
}

#[test]
fn hole_in_the_audio_lane_is_not_a_late_start_and_asks_the_page() {
    // Видео 0–4 с подряд, звук 0–1,2 с и с 2,837 с: дыра посреди звука. Цель 2 с
    // в дыре — звук не «начинается позже», а недокачан: просьба странице, звук
    // ждёт её байтов, а не вступает с 2,837 с.
    let mut stream = Stream::new();
    stream.push(VIDEO, 0..10);
    stream.push(AUDIO, 0..3);
    stream.push(AUDIO, 7..10);
    let mut demuxer = stream.open();
    let target = ms(2_000);

    demuxer.seek(target).expect("seek");

    assert_eq!(
        stream.monitor.take_page_seek_request(),
        Some(PageSeekRequest { target })
    );
    assert_eq!(
        landing(&demuxer, AUDIO),
        Some(LaneLanding::AwaitingPage(ShelfTime::from_micros(2_000_000)))
    );
    stream.push(AUDIO, 4..7);
    assert_eq!(
        landing(&demuxer, AUDIO),
        Some(LaneLanding::FromShelf(audio_fragment_start(4)))
    );
    stream.writer.end_of_stream().expect("end");
    let (video, audio) = read_to_end(&mut demuxer);
    assert_eq!(video, video_frames_from(2_000));
    assert_eq!(audio.first().copied(), Some(audio_fragment_start_ms(4)));
}

/// Обе полосы: старт фрагментами 0–2, затем (перемотка вперёд, что успела
/// прислать страница) видео с 2,4 с и звук с 2,837 с; перемотка чуть назад — к 2 с.
fn stream_with_later_time_already_on_the_shelf() -> Stream {
    let mut stream = Stream::new();
    stream.push(VIDEO, 0..3);
    stream.push(AUDIO, 0..3);
    stream.push(VIDEO, 6..10);
    stream.push(AUDIO, 7..10);
    stream
}

#[test]
fn without_a_lane_holding_the_target_data_after_it_is_not_joined() {
    // Байты обеих полос начинаются после цели: это может быть просто ещё не
    // присланное время — источник просит страницу и ждёт её байтов с цели, а не
    // вступает с того, что лежит позже.
    let mut stream = stream_with_later_time_already_on_the_shelf();
    let mut demuxer = stream.open();
    let target = ms(2_000);

    demuxer.seek(target).expect("seek");

    assert_eq!(
        stream.monitor.take_page_seek_request(),
        Some(PageSeekRequest { target })
    );
    let awaiting = Some(LaneLanding::AwaitingPage(ShelfTime::from_micros(2_000_000)));
    assert_eq!(landing(&demuxer, VIDEO), awaiting);
    assert_eq!(landing(&demuxer, AUDIO), awaiting);
}

#[test]
fn after_an_off_shelf_seek_shelved_later_time_is_not_a_join_point_whichever_lane_comes_first() {
    // Страница отвечает на просьбу временем цели: видео 2,0–2,4 с, звук с 1,62 с.
    // Какая полоса ни пришла бы первой, вторая НЕ вступает с лежавшего на полке
    // позже (видео 2,4 с / звук 2,837 с): её данные цели ещё в пути.
    for video_first in [true, false] {
        let mut stream = stream_with_later_time_already_on_the_shelf();
        let mut demuxer = stream.open();
        let target = ms(2_000);
        demuxer.seek(target).expect("seek");
        assert_eq!(
            stream.monitor.take_page_seek_request(),
            Some(PageSeekRequest { target })
        );
        let awaiting = Some(LaneLanding::AwaitingPage(ShelfTime::from_micros(2_000_000)));

        let (first, second) = if video_first {
            ((VIDEO, 5..6), (AUDIO, 4..7))
        } else {
            ((AUDIO, 4..7), (VIDEO, 5..6))
        };
        stream.push(first.0, first.1);
        assert_eq!(
            landing(&demuxer, second.0),
            awaiting,
            "video first: {video_first}"
        );
        stream.push(second.0, second.1);
        stream.writer.end_of_stream().expect("end");

        assert_eq!(
            landing(&demuxer, VIDEO),
            Some(LaneLanding::FromShelf(ShelfTime::from_micros(2_000_000)))
        );
        assert_eq!(
            landing(&demuxer, AUDIO),
            Some(LaneLanding::FromShelf(audio_fragment_start(4)))
        );
        let (video, audio) = read_to_end(&mut demuxer);
        assert_eq!(
            video,
            video_frames_from(2_000),
            "video first: {video_first}"
        );
        assert_eq!(audio.first().copied(), Some(audio_fragment_start_ms(4)));
        assert_eq!(
            stream.monitor.take_page_seek_request(),
            None,
            "one request per seek"
        );
    }
}
