//! Пределы буфера потока на границе с настоящими демуксерами: «полон» по байтам,
//! выброс после перемотки вне полки и у потолка, `changeType` в MPEG-TS, предел
//! ждущих эпизодов, шов между эпизодами (устаревшие позиции после перемотки —
//! `stream_buffer_settle_tests.rs`).
//!
//! Фикстуры: `video-h264-64.mp4` / `-96.mp4` (4 фрагмента по 0,2 с, 5 кадров по
//! 40 мс, каждый с ключевого), `video-h264-64-4s.mp4` (10 фрагментов по 0,4 с),
//! `audio-aac.mp4` (0,8 с).

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use media_core::{DemuxReadEvent, Demuxer, PacketKeyframe, TrackKind};

use super::StreamBuffer;
use crate::buffer_window::{BufferIntake, BufferWindow};
use crate::container::PushedContainer;
use crate::episode::EpisodeFactory;
use crate::error::PushedStreamError;
use crate::feed::{EpisodeKey, FeedCursor};
use crate::framing::{MediaTime, MediaTimeSpan, TimedSpan};
use crate::intake::{ChargedSegment, IntakeLedger};
use crate::lane::{LaneId, LaneMailbox};
use crate::policy::PushedSourcePolicy;
use crate::shelf::ShelfEntry;
use crate::stream::{
    OpenProgress, PushedStreamDemuxer, PushedStreamOpener, PushedStreamWriter,
    pushed_demux_registry, pushed_stream,
};
use crate::test_fixtures::{
    AUDIO_AAC, SplitMp4, VIDEO_H264_64, VIDEO_H264_64_4S, VIDEO_H264_96, split_mp4,
};
use crate::time::{PushedDuration, TimestampOffset};

const LANE: LaneId = LaneId::new(0);
/// Сколько ждать фоновых демуксеров.
const WAIT: Duration = Duration::from_secs(5);

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn policy() -> PushedSourcePolicy {
    PushedSourcePolicy::recommended().expect("policy")
}

fn stream(policy: PushedSourcePolicy) -> (PushedStreamWriter, PushedStreamOpener) {
    let (mut writer, opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(LANE, PushedContainer::FragmentedMp4)
        .expect("lane");
    writer.set_duration(PushedDuration::Finite(Duration::from_secs(4)));
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

/// Пакет глазами плеера: вид, время в мс, ключевой ли.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeenPacket {
    kind: TrackKind,
    pts_ms: u64,
    keyframe: bool,
}

/// Читает пакеты до конца потока (или ровно `limit`, если задан).
fn read(demuxer: &mut PushedStreamDemuxer, limit: Option<usize>) -> Vec<SeenPacket> {
    let deadline = Instant::now() + WAIT;
    let mut packets = Vec::new();
    loop {
        if limit.is_some_and(|limit| packets.len() >= limit) {
            return packets;
        }
        assert!(Instant::now() < deadline, "stuck after {packets:?}");
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::Packet(packet) => packets.push(SeenPacket {
                kind: packet.kind,
                pts_ms: u64::try_from(packet.pts.as_millis()).expect("small"),
                keyframe: packet.keyframe == PacketKeyframe::Keyframe,
            }),
            DemuxReadEvent::EndOfStream => {
                assert!(limit.is_none(), "unexpected end after {packets:?}");
                return packets;
            }
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            _ => {}
        }
    }
}

fn times(packets: &[SeenPacket]) -> Vec<u64> {
    packets.iter().map(|packet| packet.pts_ms).collect()
}

/// Времена с шагом 40 мс в `[from, to)`.
fn every_40_ms(from: u64, to: u64) -> Vec<u64> {
    (from..to).step_by(40).collect()
}

fn largest_fragment(media: &SplitMp4) -> usize {
    media
        .fragments
        .iter()
        .map(Vec::len)
        .max()
        .expect("fragments")
}

/// Ждёт, пока потоки эпизодов отпустят свои копии (они уходят чуть позже чтения).
fn wait_for_held_bytes(writer: &PushedStreamWriter, expected: usize) -> usize {
    let monitor = writer.monitor();
    let deadline = Instant::now() + WAIT;
    while monitor.held_segment_bytes() != expected && Instant::now() < deadline {
        thread::sleep(ms(1));
    }
    monitor.held_segment_bytes()
}

#[test]
fn respectful_page_that_stops_on_full_never_hits_the_byte_ceiling() {
    let media = split_mp4(VIDEO_H264_64_4S);
    // Потолок — init и четыре самых больших фрагмента: окно 120 с в секундах
    // никогда не наполнится, «полон» может прийти только по байтам.
    let ceiling = media.init.len() + 4 * largest_fragment(&media);
    let window = BufferWindow {
        ahead: Duration::from_secs(120),
    };
    let small = policy()
        .with_buffer_window(window)
        .expect("window")
        .with_max_held_stream_bytes(NonZeroUsize::new(ceiling).expect("non-zero"));
    assert!(small.buffer_full_at_held_bytes() < ceiling);
    let (mut writer, mut opener) = stream(small);
    let monitor = writer.monitor();
    writer.append(LANE, &media.init).expect("init");
    let mut demuxer: Option<PushedStreamDemuxer> = None;
    let mut played = Vec::new();
    let mut position = Duration::ZERO;
    let mut full_reports = 0;
    for fragment in &media.fragments {
        // Честная страница: пока «полон», не шлёт; плеер тем временем играет
        // 0,4 с и сообщает позицию.
        while monitor.buffer_status().intake == BufferIntake::Full {
            full_reports += 1;
            assert!(full_reports < 50, "the buffer never drained");
            let demuxer = demuxer.get_or_insert_with(|| open(&mut opener));
            played.extend(read(demuxer, Some(10)));
            position += ms(400);
            monitor.note_playback_position(position);
        }
        writer
            .append(LANE, fragment)
            .expect("a page that respects Full never reaches the ceiling");
    }
    writer.end_of_stream().expect("end");
    let demuxer = demuxer.get_or_insert_with(|| open(&mut opener));
    played.extend(read(demuxer, None));
    assert!(full_reports > 0, "Full by bytes was reported");
    assert_eq!(times(&played), every_40_ms(0, 4_000));
}

#[test]
fn off_shelf_seek_near_the_ceiling_makes_room_from_data_before_the_target() {
    let media = split_mp4(VIDEO_H264_64_4S);
    let ceiling = media.init.len() + 3 * largest_fragment(&media);
    let small = policy().with_max_held_stream_bytes(NonZeroUsize::new(ceiling).expect("non-zero"));
    let (mut writer, mut opener) = stream(small);
    writer.append(LANE, &media.init).expect("init");
    for fragment in &media.fragments[..3] {
        writer.append(LANE, fragment).expect("within the ceiling");
    }
    let mut demuxer = open(&mut opener);
    assert_eq!(times(&read(&mut demuxer, Some(5))), every_40_ms(0, 200));
    // Цели (2,8 с) на полке нет: полоса ждёт байтов страницы, а полка забита
    // старым временем, которое плееру больше не нужно.
    let landed = demuxer.seek(ms(2_800)).expect("seek");
    assert_eq!(landed.actual_position.as_duration(), ms(2_800));
    for fragment in &media.fragments[7..] {
        writer
            .append(LANE, fragment)
            .expect("old time before the target makes room");
    }
    writer.end_of_stream().expect("end");
    assert_eq!(times(&read(&mut demuxer, None)), every_40_ms(2_800, 4_000));
    assert_eq!(writer.failure(), None);
}

#[test]
fn ceiling_eviction_keeps_segments_whose_bytes_a_pipe_still_holds() {
    let policy = policy();
    let ledger = Arc::new(IntakeLedger::new(300));
    let factory = EpisodeFactory::new(Arc::new(pushed_demux_registry().expect("registry")), policy);
    let buffer = StreamBuffer::new(Arc::clone(&ledger), factory, policy.stream_buffer_rules());
    buffer.add_lane(
        LANE,
        PushedContainer::FragmentedMp4,
        Arc::new(LaneMailbox::default()),
        4,
    );
    let key = EpisodeKey::new(PushedContainer::FragmentedMp4, None, TimestampOffset::ZERO);
    let at = |milliseconds: i64| MediaTime::from_micros(milliseconds * 1_000);
    let mut in_pipe = Vec::new();
    {
        let mut state = buffer.lock();
        let lane = state.lanes.get_mut(&LANE).expect("lane");
        for start in [0, 200, 400] {
            let media = ChargedSegment::charge(&ledger, Bytes::from(vec![0; 100])).expect("room");
            if start == 0 {
                // Демуксер эпизода ещё не прочитал первый сегмент из трубы.
                in_pipe.push(media.clone());
            }
            let span = MediaTimeSpan::new(at(start), at(start + 200)).expect("span");
            lane.shelf.insert(ShelfEntry::waiting(
                TimedSpan::exact(span),
                media,
                key.clone(),
            ));
        }
        lane.feed.advance_cursor(FeedCursor::exact(at(600)));
        state.reported_position = Some(at(600));
    }
    // Всё показано; место нужно под 100 байтов. Снимать сегмент из трубы
    // бессмысленно (память не освободится) — снимается следующий.
    let charged = buffer
        .charge(Bytes::from(vec![0; 100]))
        .expect("room after eviction");
    let layout: Vec<i64> = buffer.lock().lanes[&LANE]
        .shelf
        .layout()
        .iter()
        .map(|(start, _, _)| start / 1_000)
        .collect();
    assert_eq!(
        layout,
        [0, 400],
        "the segment still in the pipe stays on the shelf"
    );
    assert_eq!(ledger.held_bytes(), 300);
    drop((charged, in_pipe));
}

#[test]
fn change_to_mpeg_ts_releases_the_timed_shelf_with_exact_accounting() {
    let media = split_mp4(VIDEO_H264_64);
    let (mut writer, mut opener) = stream(policy());
    writer.append(LANE, &media.init).expect("init");
    // Фрагмент 3 за дырой: его подача так и не начнётся.
    for fragment in [
        &media.fragments[0],
        &media.fragments[1],
        &media.fragments[3],
    ] {
        writer.append(LANE, fragment).expect("fragment");
    }
    let monitor = writer.monitor();
    assert!(monitor.held_segment_bytes() > 0);
    writer
        .change_container(LANE, PushedContainer::MpegTs)
        .expect("changeType");
    writer.end_of_stream().expect("end");
    // Поданное доигрывается; ждавший за дырой фрагмент не играется.
    assert_eq!(
        times(&read(&mut open(&mut opener), None)),
        every_40_ms(0, 400)
    );
    // Полка отпущена целиком: после дочитывания поток не держит ни байта.
    assert_eq!(wait_for_held_bytes(&writer, 0), 0);
}

#[test]
fn six_quality_switches_ahead_of_the_player_all_play_in_order() {
    let low = split_mp4(VIDEO_H264_64);
    let high = split_mp4(VIDEO_H264_96);
    let (mut writer, mut opener) = stream(policy());
    // Семь сегментов подряд, init чередуется (64 ↔ 96): шесть смен качества
    // впереди плеера — семь эпизодов, а ящик держит четыре.
    for step in 0..7_u64 {
        let quality = if step % 2 == 0 { &low } else { &high };
        writer
            .set_timestamp_offset(
                LANE,
                TimestampOffset::from_micros(i64::try_from(step * 200_000).expect("small")),
            )
            .expect("offset");
        writer.append(LANE, &quality.init).expect("init");
        writer
            .append(LANE, &quality.fragments[0])
            .expect("a quality switch ahead never kills the stream");
    }
    assert_eq!(writer.failure(), None);
    writer.end_of_stream().expect("end");
    let played = read(&mut open(&mut opener), None);
    assert_eq!(times(&played), every_40_ms(0, 1_400));
}

#[test]
fn straddling_replacement_never_sends_time_backwards() {
    let low = split_mp4(VIDEO_H264_64);
    let high = split_mp4(VIDEO_H264_96);
    let (mut writer, mut opener) = stream(policy());
    writer.append(LANE, &low.init).expect("init");
    writer.append(LANE, &low.fragments[0]).expect("0–200");
    writer.append(LANE, &low.fragments[1]).expect("200–400");
    // Смена качества с другой нарезкой: B[300, 500] перешагивает поданное до 400.
    writer
        .set_timestamp_offset(LANE, TimestampOffset::from_micros(100_000))
        .expect("offset");
    writer.append(LANE, &high.init).expect("high init");
    writer.append(LANE, &high.fragments[1]).expect("300–500");
    writer.append(LANE, &high.fragments[2]).expect("500–700");
    writer.end_of_stream().expect("end");
    let played = read(&mut open(&mut opener), None);
    let played_times = times(&played);
    assert!(
        played_times.windows(2).all(|pair| pair[0] < pair[1]),
        "time went backwards or repeated: {played_times:?}"
    );
    // Кадры B 300–360 — повтор; 400–460 зависят от выброшенного ключевого 300 —
    // видео ждёт следующий ключевой кадр (500).
    let mut expected = every_40_ms(0, 400);
    expected.extend(every_40_ms(500, 700));
    assert_eq!(played_times, expected);
    let first_after_seam = played.iter().find(|packet| packet.pts_ms == 500);
    assert!(first_after_seam.is_some_and(|packet| packet.keyframe));
}

#[test]
fn straddling_audio_replacement_is_not_heard_twice() {
    let audio = split_mp4(AUDIO_AAC);
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(LANE, PushedContainer::FragmentedMp4)
        .expect("lane");
    writer.append(LANE, &audio.init).expect("init");
    writer.append(LANE, &audio.fragments[0]).expect("0");
    writer.append(LANE, &audio.fragments[1]).expect("1");
    // Тот же звук ещё раз, со сдвигом на 100 мс: перешагивает поданное.
    writer
        .set_timestamp_offset(LANE, TimestampOffset::from_micros(100_000))
        .expect("offset");
    writer
        .append(LANE, &audio.fragments[1])
        .expect("1 again, shifted");
    writer.end_of_stream().expect("end");
    let played = read(&mut open(&mut opener), None);
    assert!(played.iter().all(|packet| packet.kind == TrackKind::Audio));
    let played_times = times(&played);
    assert!(
        played_times.windows(2).all(|pair| pair[0] < pair[1]),
        "audio repeated: {played_times:?}"
    );
    // Звук сдвинутого повтора, который позже поданного, — слышен.
    assert!(played_times.last().is_some_and(|last| *last > 400));
}

#[test]
fn position_reported_before_a_seek_is_ignored_until_one_near_the_target() {
    let media = split_mp4(VIDEO_H264_64);
    let window = BufferWindow {
        ahead: Duration::from_secs(120),
    };
    let (mut writer, mut opener) = stream(policy().with_buffer_window(window).expect("window"));
    let monitor = writer.monitor();
    writer.append(LANE, &media.init).expect("init");
    for fragment in &media.fragments {
        writer.append(LANE, fragment).expect("fragment");
    }
    let mut demuxer = open(&mut opener);
    read(&mut demuxer, Some(3));
    demuxer.seek(ms(200)).expect("seek on the shelf");
    let held = monitor.held_segment_bytes();
    // Позиция старого места (снята до перемотки) пришла после неё.
    monitor.note_playback_position(ms(100_000));
    assert_eq!(monitor.buffer_status().buffered_ahead, ms(600));
    assert_eq!(monitor.held_segment_bytes(), held, "nothing evicted");
    // Позиция у цели принимается: запас считается от неё.
    monitor.note_playback_position(ms(300));
    assert_eq!(monitor.buffer_status().buffered_ahead, ms(500));
}

#[test]
fn bytes_ceiling_is_still_an_error_for_a_page_that_ignores_full() {
    let media = split_mp4(VIDEO_H264_64_4S);
    let ceiling = media.init.len() + 2 * largest_fragment(&media);
    let small = policy().with_max_held_stream_bytes(NonZeroUsize::new(ceiling).expect("non-zero"));
    let (mut writer, _opener) = stream(small);
    writer.append(LANE, &media.init).expect("init");
    let refused = media
        .fragments
        .iter()
        .map(|fragment| writer.append(LANE, fragment))
        .find_map(Result::err);
    assert_eq!(
        refused,
        Some(PushedStreamError::IntakeLimitExceeded { limit: ceiling })
    );
}
