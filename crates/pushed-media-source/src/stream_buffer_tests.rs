//! Тесты полки на границе потока: порядок подачи, дыры, повторы, окно, потолок,
//! «буферизовано»/«полон» и байты после `endOfStream` — с настоящими демуксерами.
//!
//! Фикстура `video-h264-64.mp4`: init + 4 фрагмента по 200 мс (5 кадров по 40 мс).

use std::num::NonZeroUsize;
use std::thread;
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, Demuxer};

use crate::buffer_window::{BufferIntake, BufferStatus, BufferWindow};
use crate::container::PushedContainer;
use crate::error::PushedStreamError;
use crate::lane::LaneId;
use crate::monitor::PushedStreamMonitor;
use crate::policy::{PushedSourcePolicy, PushedSourcePolicyError};
use crate::stream::{
    OpenProgress, PushedStreamDemuxer, PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use crate::test_fixtures::{SplitMp4, VIDEO_H264_64, split_mp4};
use crate::time::TimestampOffset;

const VIDEO: LaneId = LaneId::new(0);
const SECOND: LaneId = LaneId::new(1);
/// Сколько ждать фоновых демуксеров.
const WAIT: Duration = Duration::from_secs(5);
/// Длительность кадра фикстуры.
const FRAME_MS: u64 = 40;

fn fixture() -> SplitMp4 {
    let media = split_mp4(VIDEO_H264_64);
    assert_eq!(media.fragments.len(), 4);
    media
}

fn video_stream(policy: PushedSourcePolicy) -> (PushedStreamWriter, PushedStreamOpener) {
    let (mut writer, opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("lane");
    (writer, opener)
}

fn policy() -> PushedSourcePolicy {
    PushedSourcePolicy::recommended().expect("policy")
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

/// Читает пакеты (время в мс) до конца потока.
fn read_to_end(demuxer: &mut PushedStreamDemuxer) -> Vec<u64> {
    let deadline = Instant::now() + WAIT;
    let mut times = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "stream did not end: {times:?}");
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::Packet(packet) => {
                times.push(u64::try_from(packet.pts.as_millis()).expect("small"));
            }
            DemuxReadEvent::EndOfStream => return times,
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            _ => {}
        }
    }
}

/// Читает ровно `count` пакетов (время в мс); не дождался — тест падает.
fn read_packets(demuxer: &mut PushedStreamDemuxer, count: usize) -> Vec<u64> {
    let deadline = Instant::now() + WAIT;
    let mut times = Vec::new();
    while times.len() < count {
        assert!(Instant::now() < deadline, "only {times:?} arrived");
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::Packet(packet) => {
                times.push(u64::try_from(packet.pts.as_millis()).expect("small"));
            }
            DemuxReadEvent::EndOfStream => panic!("unexpected end after {times:?}"),
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            _ => {}
        }
    }
    times
}

/// Плеер не получает пакетов `quiet` подряд (ждёт на дыре).
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

/// Кадры фрагментов `range` (по 5 на фрагмент), время в мс.
fn frames_of(fragments: std::ops::Range<u64>) -> Vec<u64> {
    (fragments.start * 5..fragments.end * 5)
        .map(|frame| frame * FRAME_MS)
        .collect()
}

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

#[test]
fn out_of_order_fragments_are_fed_in_time_order_exactly_once() {
    let media = fixture();
    let (mut writer, mut opener) = video_stream(policy());
    let monitor = writer.monitor();
    writer.append(VIDEO, &media.init).expect("init");
    writer.append(VIDEO, &media.fragments[0]).expect("0");
    // 2 и 3 пришли раньше 1: ждут на полке за дырой, запас — только фрагмент 0.
    writer.append(VIDEO, &media.fragments[2]).expect("2");
    writer.append(VIDEO, &media.fragments[3]).expect("3");
    assert_eq!(monitor.buffer_status().buffered_ahead, ms(200));
    // 1 закрывает дыру: подаются 1, 2, 3 — по времени, а не по приходу.
    writer.append(VIDEO, &media.fragments[1]).expect("1");
    assert_eq!(monitor.buffer_status().buffered_ahead, ms(800));
    writer.end_of_stream().expect("end");
    assert_eq!(read_to_end(&mut open(&mut opener)), frames_of(0..4));
}

#[test]
fn gap_ahead_makes_the_player_wait_until_it_is_filled() {
    let media = fixture();
    let (mut writer, mut opener) = video_stream(policy());
    writer.append(VIDEO, &media.init).expect("init");
    writer.append(VIDEO, &media.fragments[0]).expect("0");
    writer
        .append(VIDEO, &media.fragments[2])
        .expect("2 after a gap");
    let mut demuxer = open(&mut opener);
    assert_eq!(read_packets(&mut demuxer, 5), frames_of(0..1));
    // Фрагмент 2 на полке, но демуксеру не подан: плеер ждёт, а не прыгает.
    assert_waits(&mut demuxer, ms(100));
    // Доказательство, а не «за 100 мс ничего не пришло»: даже конец входа не
    // подаёт фрагмент за дырой — плеер доигрывает поданное и получает конец.
    writer
        .end_of_stream()
        .expect("end before the gap is filled");
    assert_eq!(read_to_end(&mut demuxer), [] as [u64; 0]);
    // Дыру закрыли (MSE: байты после `endOfStream` снова открывают вход) —
    // только теперь подаются 1 и 2, по порядку.
    writer
        .append(VIDEO, &media.fragments[1])
        .expect("1 fills the gap");
    writer.end_of_stream().expect("end");
    assert_eq!(read_to_end(&mut demuxer), frames_of(1..3));
}

#[test]
fn resent_past_segment_is_shelved_not_played_twice() {
    let media = fixture();
    let (mut writer, mut opener) = video_stream(policy());
    for fragment in &media.fragments[..3] {
        writer.append(VIDEO, &media.init).expect("init");
        writer.append(VIDEO, fragment).expect("fragment");
    }
    // Страница перемоталась назад сама (как Twitch до `seek_started`) и шлёт
    // прошлое: фрагмент 0 заменяет свою копию на полке, но не подаётся снова.
    writer.append(VIDEO, &media.fragments[0]).expect("past");
    let shelved: usize =
        media.init.len() + media.fragments[..3].iter().map(Vec::len).sum::<usize>();
    // Заменённая копия фрагмента 0 ещё может лежать в трубе эпизода, пока его
    // поток её не прочитал (это настоящая память) — ждём, пока прочитает: после
    // этого поток держит ровно одну копию каждого фрагмента.
    let monitor = writer.monitor();
    let deadline = Instant::now() + WAIT;
    while monitor.held_segment_bytes() != shelved && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(monitor.held_segment_bytes(), shelved);
    writer.end_of_stream().expect("end");
    assert_eq!(read_to_end(&mut open(&mut opener)), frames_of(0..3));
}

#[test]
fn played_segments_beyond_the_behind_window_stay_while_the_page_holds_them() {
    let media = fixture();
    let window = BufferWindow {
        ahead: Duration::from_secs(1),
        behind: ms(200),
    };
    let (mut writer, mut opener) =
        video_stream(policy().with_buffer_window(window).expect("window"));
    let monitor = writer.monitor();
    writer.append(VIDEO, &media.init).expect("init");
    for fragment in &media.fragments {
        writer.append(VIDEO, fragment).expect("fragment");
    }
    writer.end_of_stream().expect("end");
    // Демуксер дочитал трубу: остаются только полка и init.
    assert_eq!(read_to_end(&mut open(&mut opener)).len(), 20);
    let all = media.init.len() + media.fragments.iter().map(Vec::len).sum::<usize>();
    assert_eq!(monitor.held_segment_bytes(), all);
    // «Помним, пока помнит страница»: окно «назад» (200 мс) — не окно выселения.
    // Памяти хватает, страница ничего не стирала — полка держит всё сыгранное.
    monitor.note_playback_position(ms(500));
    monitor.note_playback_position(ms(1_000));
    assert_eq!(monitor.held_segment_bytes(), all);
    assert_eq!(writer.held_bytes(), all);
}

#[test]
fn ceiling_first_evicts_played_segments_then_refuses() {
    let media = fixture();
    let fragment = &media.fragments[0];
    // Потолок: init и ровно три копии фрагмента.
    let ceiling = media.init.len() + 3 * fragment.len();
    let small = policy().with_max_held_stream_bytes(NonZeroUsize::new(ceiling).expect("non-zero"));
    let (mut writer, mut opener) = video_stream(small);
    let monitor = writer.monitor();
    writer.append(VIDEO, &media.init).expect("init");
    // Тот же фрагмент (0–200 мс), сдвинутый к курсору: 0–200, 200–400, 400–600.
    for step in 0..3_i64 {
        writer
            .set_timestamp_offset(VIDEO, TimestampOffset::from_micros(step * 200_000))
            .expect("offset");
        writer.append(VIDEO, fragment).expect("within the ceiling");
    }
    assert_eq!(monitor.held_segment_bytes(), ceiling);
    let mut demuxer = open(&mut opener);
    assert_eq!(read_packets(&mut demuxer, 15), frames_of(0..3));
    monitor.note_playback_position(ms(300));
    // Над потолком: 0–200 мс уже показан (хоть и внутри окна «назад» 30 с) —
    // выбрасывается он, и новый сегмент принят.
    writer
        .set_timestamp_offset(VIDEO, TimestampOffset::from_micros(600_000))
        .expect("offset");
    writer.append(VIDEO, fragment).expect("room after eviction");
    assert_eq!(monitor.held_segment_bytes(), ceiling);
    // Дальше выбрасывать нечего: 200–400 мс ещё не показан (позиция 300 мс).
    writer
        .set_timestamp_offset(VIDEO, TimestampOffset::from_micros(800_000))
        .expect("offset");
    assert_eq!(
        writer.append(VIDEO, fragment),
        Err(PushedStreamError::IntakeLimitExceeded { limit: ceiling })
    );
}

#[test]
fn buffer_status_reports_exact_ahead_minimum_over_lanes_and_full() {
    let media = fixture();
    let window = BufferWindow {
        ahead: ms(400),
        behind: Duration::ZERO,
    };
    let (mut writer, _opener) =
        pushed_stream(policy().with_buffer_window(window).expect("window")).expect("stream");
    let monitor = writer.monitor();
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("first");
    writer
        .add_lane(SECOND, PushedContainer::FragmentedMp4)
        .expect("second");
    // Ничего не подано — позиции нет.
    assert_eq!(monitor.buffer_status(), BufferStatus::EMPTY);
    writer.append(VIDEO, &media.init).expect("init");
    for fragment in &media.fragments {
        writer.append(VIDEO, fragment).expect("fragment");
    }
    // Вторая полоса пуста: её запас ноль, а поток — минимум по полосам.
    assert_eq!(monitor.buffer_status().buffered_ahead, Duration::ZERO);
    writer.append(SECOND, &media.init).expect("init");
    writer.append(SECOND, &media.fragments[0]).expect("0");
    writer.append(SECOND, &media.fragments[1]).expect("1");
    // Позицию ещё не сообщили: она — начало первого поданного (0). Минимум 800 и
    // 400 мс — 400 мс, это ровно окно: «полон».
    assert_eq!(
        monitor.buffer_status(),
        BufferStatus {
            buffered_ahead: ms(400),
            intake: BufferIntake::Full,
        }
    );
    monitor.note_playback_position(ms(100));
    assert_eq!(
        monitor.buffer_status(),
        BufferStatus {
            buffered_ahead: ms(300),
            intake: BufferIntake::Accepting,
        }
    );
    // Позиция за концом второй полосы: она позицию не покрывает — запас ноль.
    monitor.note_playback_position(ms(500));
    assert_eq!(monitor.buffer_status().buffered_ahead, Duration::ZERO);
}

#[test]
fn bytes_after_end_of_stream_reopen_input_and_play_in_a_new_episode() {
    let media = fixture();
    let (mut writer, mut opener) = video_stream(policy());
    writer.append(VIDEO, &media.init).expect("init");
    writer.append(VIDEO, &media.fragments[0]).expect("0");
    writer.append(VIDEO, &media.fragments[1]).expect("1");
    writer.end_of_stream().expect("end");
    // MSE: `appendBuffer` после `endOfStream` снова открывает поток.
    writer.append(VIDEO, &media.fragments[2]).expect("reopens");
    writer
        .append(VIDEO, &media.fragments[3])
        .expect("continues");
    writer.end_of_stream().expect("end again");
    assert_eq!(writer.failure(), None);
    assert_eq!(read_to_end(&mut open(&mut opener)), frames_of(0..4));
}

#[test]
fn zero_ahead_window_is_refused_and_monitor_crosses_threads() {
    let zero = BufferWindow {
        ahead: Duration::ZERO,
        behind: Duration::from_secs(1),
    };
    assert_eq!(
        policy().with_buffer_window(zero).err(),
        Some(PushedSourcePolicyError::ZeroBufferAhead)
    );
    let defaults = policy().buffer_window();
    assert_eq!(
        (defaults.ahead, defaults.behind),
        (Duration::from_secs(120), Duration::from_secs(30))
    );
    fn assert_send_sync<T: Send + Sync + Clone>() {}
    assert_send_sync::<PushedStreamMonitor>();
}
