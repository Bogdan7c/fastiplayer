//! Перемотка на границе демуксера потока: полка, страница, эпохи, ключевой
//! кадр, «можно ли», неблокируемость — с настоящими демуксерами эпизодов.
//!
//! Фикстура `video-h264-64.mp4`: init + 4 фрагмента по 200 мс (5 кадров по
//! 40 мс, каждый фрагмент с ключевого кадра).

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekability, Demuxer, MediaTime, PacketKeyframe,
    TimelineNotSeekableReason,
};

use crate::container::PushedContainer;
use crate::episode::EpisodeFactory;
use crate::intake::IntakeLedger;
use crate::lane::{LaneId, LaneIntake, LaneMailbox};
use crate::page_seek::PageSeekRequest;
use crate::policy::PushedSourcePolicy;
use crate::stream::{
    OpenProgress, PushedStreamDemuxer, PushedStreamOpener, PushedStreamWriter,
    pushed_demux_registry, pushed_stream,
};
use crate::stream_buffer::StreamBuffer;
use crate::test_fixtures::{
    SplitMp4, VIDEO_H264_64, VIDEO_H264_64_MIDGOP, VIDEO_H264_96, split_mp4,
};
use crate::time::PushedDuration;

const VIDEO: LaneId = LaneId::new(0);
/// Сколько ждать фоновых демуксеров.
const WAIT: Duration = Duration::from_secs(5);

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn policy() -> PushedSourcePolicy {
    PushedSourcePolicy::recommended().expect("policy")
}

/// Поток с одной полосой fMP4 и конечной длительностью 0,8 с.
fn video_stream() -> (PushedStreamWriter, PushedStreamOpener) {
    let (mut writer, opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("lane");
    writer.set_duration(PushedDuration::Finite(ms(800)));
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

/// Читает пакеты (время в мс, ключевой ли) до конца потока.
fn read_to_end(demuxer: &mut PushedStreamDemuxer) -> Vec<(u64, bool)> {
    let deadline = Instant::now() + WAIT;
    let mut packets = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "no end: {packets:?}");
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::Packet(packet) => packets.push((
                u64::try_from(packet.pts.as_millis()).expect("small"),
                packet.keyframe == PacketKeyframe::Keyframe,
            )),
            DemuxReadEvent::EndOfStream => return packets,
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            _ => {}
        }
    }
}

fn times(packets: &[(u64, bool)]) -> Vec<u64> {
    packets.iter().map(|(time, _)| *time).collect()
}

/// Кадры с `from_ms` до конца фикстуры (40 мс шаг).
fn frames_from(from_ms: u64) -> Vec<u64> {
    (from_ms / 40..20).map(|frame| frame * 40).collect()
}

/// Читает ровно `count` пакетов.
fn read_packets(demuxer: &mut PushedStreamDemuxer, count: usize) -> Vec<u64> {
    let deadline = Instant::now() + WAIT;
    let mut read = Vec::new();
    while read.len() < count {
        assert!(Instant::now() < deadline, "only {read:?} arrived");
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::Packet(packet) => {
                read.push(u64::try_from(packet.pts.as_millis()).expect("small"));
            }
            DemuxReadEvent::EndOfStream => panic!("unexpected end after {read:?}"),
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            _ => {}
        }
    }
    read
}

/// Плеер не получает ни пакета, ни конца `quiet` подряд.
fn assert_waits(demuxer: &mut PushedStreamDemuxer, quiet: Duration) {
    let until = Instant::now() + quiet;
    while Instant::now() < until {
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::Packet(packet) => panic!("unexpected packet at {:?}", packet.pts),
            DemuxReadEvent::EndOfStream => panic!("unexpected end of stream"),
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            _ => {}
        }
    }
}

fn push_all(writer: &mut PushedStreamWriter, media: &SplitMp4) {
    writer.append(VIDEO, &media.init).expect("init");
    for fragment in &media.fragments {
        writer.append(VIDEO, fragment).expect("fragment");
    }
}

#[test]
fn on_shelf_seek_restarts_at_the_fragment_holding_the_target_and_drops_the_old_position() {
    let media = split_mp4(VIDEO_H264_64);
    let (mut writer, mut opener) = video_stream();
    let monitor = writer.monitor();
    push_all(&mut writer, &media);
    writer.end_of_stream().expect("end");
    let mut demuxer = open(&mut opener);
    assert_eq!(read_packets(&mut demuxer, 3), [0, 40, 80]);

    let landed = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(ms(500)))
        .expect("seek");
    assert_eq!(landed.requested_position, MediaTime::from_duration(ms(500)));
    // Начало фрагмента с целью (400–600 мс) — безопасная точка не позже цели.
    assert_eq!(landed.actual_position, MediaTime::from_duration(ms(400)));
    // Ни одного пакета старой позиции (120–360 мс) после перемотки.
    assert_eq!(times(&read_to_end(&mut demuxer)), frames_from(400));
    assert_eq!(monitor.take_page_seek_request(), None);
}

#[test]
fn seek_after_end_of_stream_replays_from_the_shelf() {
    let media = split_mp4(VIDEO_H264_64);
    let (mut writer, mut opener) = video_stream();
    push_all(&mut writer, &media);
    writer.end_of_stream().expect("end");
    let mut demuxer = open(&mut opener);
    assert_eq!(times(&read_to_end(&mut demuxer)), frames_from(0));

    demuxer.seek(ms(200)).expect("seek after the end");
    assert_eq!(times(&read_to_end(&mut demuxer)), frames_from(200));
}

#[test]
fn seek_at_or_past_the_end_after_end_of_stream_lands_on_the_last_segment() {
    let media = split_mp4(VIDEO_H264_64);
    let (mut writer, mut opener) = video_stream();
    let monitor = writer.monitor();
    push_all(&mut writer, &media);
    writer.end_of_stream().expect("end");
    let mut demuxer = open(&mut opener);
    assert_eq!(times(&read_to_end(&mut demuxer)), frames_from(0));

    // На длительность (0,8 с) и дальше: страница уже сказала «байтов больше
    // нет» — полоса садится на последний фрагмент и отдаёт конец потока, а не
    // ждёт вечно и не просит страницу.
    for target in [800, 5_000] {
        let landed = demuxer.seek(ms(target)).expect("seek past the end");
        assert_eq!(landed.actual_position, MediaTime::from_duration(ms(600)));
        assert_eq!(
            times(&read_to_end(&mut demuxer)),
            frames_from(600),
            "{target}"
        );
        assert_eq!(monitor.take_page_seek_request(), None, "{target}");
    }
}

#[test]
fn off_shelf_seek_asks_the_page_and_feeds_only_from_the_target_segment() {
    let media = split_mp4(VIDEO_H264_64);
    let (mut writer, mut opener) = video_stream();
    let monitor = writer.monitor();
    writer.append(VIDEO, &media.init).expect("init");
    writer.append(VIDEO, &media.fragments[0]).expect("0");
    let mut demuxer = open(&mut opener);

    let landed = demuxer.seek(ms(600)).expect("seek");
    // Байтов с цели нет: пакеты пойдут с самой цели.
    assert_eq!(landed.actual_position, MediaTime::from_duration(ms(600)));
    assert_eq!(
        monitor.take_page_seek_request(),
        Some(PageSeekRequest { target: ms(600) })
    );
    assert_eq!(monitor.take_page_seek_request(), None, "taken once");
    assert_waits(&mut demuxer, ms(100));
    // Страница досылает старое время (кончается до цели) — на полку, не плееру;
    // даже фрагмент 1, который продолжал бы старую подачу.
    writer.append(VIDEO, &media.fragments[1]).expect("1");
    writer.append(VIDEO, &media.fragments[2]).expect("2");
    assert_waits(&mut demuxer, ms(100));
    writer.append(VIDEO, &media.fragments[3]).expect("3");
    writer.end_of_stream().expect("end");
    assert_eq!(times(&read_to_end(&mut demuxer)), frames_from(600));
}

#[test]
fn video_waits_for_a_keyframe_after_a_seek_into_a_fragment_without_one() {
    let media = split_mp4(VIDEO_H264_64_MIDGOP);
    assert_eq!(media.fragments.len(), 4);
    let (mut writer, mut opener) = video_stream();
    push_all(&mut writer, &media);
    writer.end_of_stream().expect("end");
    let mut demuxer = open(&mut opener);
    assert_eq!(read_packets(&mut demuxer, 2), [0, 40]);

    // Цель во фрагменте 1 (200–400 мс), а он начинается не с ключевого кадра:
    // без ключевого декодер показал бы мусор, поэтому первый пакет — ключевой.
    demuxer.seek(ms(280)).expect("seek");
    let packets = read_to_end(&mut demuxer);
    assert_eq!(packets.first(), Some(&(400, true)), "{packets:?}");
    assert_eq!(times(&packets), frames_from(400));
}

#[test]
fn seek_never_waits_for_an_episode_thread_blocked_on_its_pipe() {
    let media = split_mp4(VIDEO_H264_64);
    let (mut writer, mut opener) = video_stream();
    writer.append(VIDEO, &media.init).expect("init");
    writer.append(VIDEO, &media.fragments[0]).expect("0");
    writer.append(VIDEO, &media.fragments[1]).expect("1");
    let mut demuxer = open(&mut opener);
    // Демуксер эпизода прочитал всё и теперь спит на трубе, ожидая байтов.
    assert_eq!(read_packets(&mut demuxer, 10).len(), 10);
    assert_waits(&mut demuxer, ms(50));

    let (done, finished) = mpsc::channel();
    let seeking = thread::spawn(move || {
        let landed = demuxer.seek(ms(100)).map(|landed| landed.actual_position);
        done.send(landed.is_ok()).expect("report");
        demuxer
    });
    // `join` потока эпизода под замком никогда бы не вернулся.
    assert_eq!(finished.recv_timeout(ms(1_000)), Ok(true));
    let mut demuxer = seeking.join().expect("seek thread");
    writer.end_of_stream().expect("end");
    assert_eq!(
        times(&read_to_end(&mut demuxer)),
        [0, 40, 80, 120, 160, 200, 240, 280, 320, 360]
    );
}

#[test]
fn seekability_follows_timestamps_and_duration() {
    let media = split_mp4(VIDEO_H264_64);
    for (duration, expected) in [
        (PushedDuration::Finite(ms(800)), DemuxSeekability::Seekable),
        (
            PushedDuration::Unknown,
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::UnknownTimeline,
            },
        ),
        (
            PushedDuration::Live,
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::LiveWindowUnavailable,
            },
        ),
    ] {
        let (mut writer, mut opener) = video_stream();
        writer.set_duration(duration);
        push_all(&mut writer, &media);
        let demuxer = open(&mut opener);
        assert_eq!(demuxer.seekability(), expected, "{duration:?}");
    }
}

#[test]
fn mpeg_ts_streams_cannot_seek() {
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::MpegTs)
        .expect("lane");
    writer.set_duration(PushedDuration::Finite(ms(400)));
    writer
        .append(VIDEO, include_bytes!("../fixtures/muxed-h264-aac.ts"))
        .expect("ts");
    writer.end_of_stream().expect("end");
    let mut demuxer = open(&mut opener);
    assert!(matches!(
        demuxer.seekability(),
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable
        }
    ));
    let error = demuxer.seek(ms(100)).expect_err("no timestamps to seek by");
    assert!(format!("{error:#}").contains("no timestamps"), "{error:#}");
    // Отказ перемотки — не ошибка потока: он доигрывается.
    assert!(!read_to_end(&mut demuxer).is_empty());
    assert_eq!(writer.failure(), None);
}

#[test]
fn stale_episodes_are_handed_to_the_caller_outside_the_buffer_lock() {
    let policy = policy();
    let mailbox = Arc::new(LaneMailbox::default());
    let buffer = Arc::new(StreamBuffer::new(
        Arc::new(IntakeLedger::new(1 << 22)),
        EpisodeFactory::new(Arc::new(pushed_demux_registry().expect("registry")), policy),
        policy.stream_buffer_rules(),
    ));
    buffer.add_lane(
        VIDEO,
        PushedContainer::FragmentedMp4,
        Arc::clone(&mailbox),
        policy.max_pending_episodes_per_lane(),
    );
    let mut intake = LaneIntake::new(
        VIDEO,
        PushedContainer::FragmentedMp4,
        policy.framing_limits(),
        Arc::clone(&mailbox),
        Arc::clone(&buffer),
    );
    // Два эпизода ждут плеер: 64×64 (0–200 мс) и смена качества 96×96 (200–400 мс).
    let (low, high) = (split_mp4(VIDEO_H264_64), split_mp4(VIDEO_H264_96));
    intake.append(&low.init).expect("init");
    intake.append(&low.fragments[0]).expect("0");
    intake.append(&high.init).expect("new init");
    intake.append(&high.fragments[1]).expect("1");
    assert_eq!(mailbox.pending_episodes(), 2);

    let repositioned = buffer.seek(ms(300)).expect("seek");
    assert_eq!(repositioned.stale.len(), 2);
    // Старые эпизоды ещё живы у вызывающего, а замок буфера свободен.
    assert_eq!(buffer.buffer_status().buffered_ahead, ms(100));
    // В ящике — только эпизод новой эпохи, начатый с сегмента с целью.
    assert_eq!(mailbox.pending_episodes(), 1);
    drop(repositioned);
}
