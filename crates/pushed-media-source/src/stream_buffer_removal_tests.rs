//! «Помним, пока помнит страница» на границе потока, с настоящими демуксерами:
//! стирание страницей (`note_page_removed`) и предохранитель памяти у порога
//! «полон».
//!
//! Фикстуры: `video-h264-64.mp4` (4 фрагмента по 0,2 с, 5 кадров по 40 мс) и
//! `video-h264-64-4s.mp4` (10 фрагментов по 0,4 с), каждый фрагмент — с ключевого.

use std::num::NonZeroUsize;
use std::thread;
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, Demuxer};

use crate::buffer_window::{BufferIntake, BufferWindow};
use crate::container::PushedContainer;
use crate::error::PushedStreamError;
use crate::lane::LaneId;
use crate::page_removal::{
    PageRemovalOutcome, PageRemovalReport, PageRemovedRange, PageRemovedRangeEnd,
};
use crate::page_seek::PageSeekRequest;
use crate::policy::PushedSourcePolicy;
use crate::stream::{
    OpenProgress, PushedStreamDemuxer, PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use crate::test_fixtures::{SplitMp4, VIDEO_H264_64, VIDEO_H264_64_4S, split_mp4};
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
    // Перемотка требует известной длительности.
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

/// Читает ровно `count` пакетов; не дождался — тест падает.
fn read_packets(demuxer: &mut PushedStreamDemuxer, count: usize) {
    let deadline = Instant::now() + WAIT;
    let mut read = 0;
    while read < count {
        assert!(Instant::now() < deadline, "only {read} packets arrived");
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::Packet(_) => read += 1,
            DemuxReadEvent::EndOfStream => panic!("unexpected end after {read} packets"),
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            _ => {}
        }
    }
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

fn removal(start: Duration, end: PageRemovedRangeEnd) -> PageRemovedRange {
    PageRemovedRange::new(start, end)
}

fn applied(outcome: Result<PageRemovalOutcome, PushedStreamError>) -> PageRemovalReport {
    match outcome.expect("removal accepted") {
        PageRemovalOutcome::Applied(report) => report,
        PageRemovalOutcome::UntimedLane => panic!("a timed lane"),
    }
}

/// Поток из всех четырёх фрагментов `video-h264-64.mp4`, сыгранный до позиции
/// `position`; демуксер дочитал трубу, поток держит полку и init.
fn played_stream(position: Duration) -> (SplitMp4, PushedStreamWriter, PushedStreamDemuxer) {
    let media = split_mp4(VIDEO_H264_64);
    let (mut writer, mut opener) = stream(policy());
    writer.append(LANE, &media.init).expect("init");
    for fragment in &media.fragments {
        writer.append(LANE, fragment).expect("fragment");
    }
    let mut demuxer = open(&mut opener);
    read_packets(&mut demuxer, 20);
    let all = media.init.len() + media.fragments.iter().map(Vec::len).sum::<usize>();
    assert_eq!(wait_for_held_bytes(&writer, all), all);
    writer.monitor().note_playback_position(position);
    (media, writer, demuxer)
}

#[test]
fn page_removal_of_played_past_releases_exactly_those_segments_and_a_seek_there_asks_the_page() {
    let (media, mut writer, mut demuxer) = played_stream(ms(800));
    let monitor = writer.monitor();
    let held = monitor.held_segment_bytes();
    // Страница стёрла у себя 0–400 мс: это два сыгранных фрагмента.
    let report =
        applied(writer.note_page_removed(LANE, removal(ms(0), PageRemovedRangeEnd::At(ms(400)))));
    let released = media.fragments[0].len() + media.fragments[1].len();
    assert_eq!(
        report,
        PageRemovalReport {
            released_segments: 2,
            released_bytes: released,
            ..PageRemovalReport::default()
        }
    );
    assert_eq!(monitor.held_segment_bytes(), held - released);
    // Туда, где страница стёрла, перемотка идёт к странице: у полки этого нет.
    demuxer.seek(ms(100)).expect("seek");
    assert_eq!(
        monitor.take_page_seek_request(),
        Some(PageSeekRequest { target: ms(100) })
    );
    // А нестёртое прошлое (400–800 мс) по-прежнему на полке.
    demuxer.seek(ms(500)).expect("seek");
    assert_eq!(monitor.take_page_seek_request(), None);
}

#[test]
fn page_removal_ahead_partial_or_of_an_unknown_lane_releases_nothing() {
    let (_media, mut writer, _demuxer) = played_stream(ms(300));
    let monitor = writer.monitor();
    let held = monitor.held_segment_bytes();
    // Впереди позиции (300 мс): 400–800 мс ещё не сыграны.
    let ahead = applied(
        writer.note_page_removed(LANE, removal(ms(400), PageRemovedRangeEnd::EndOfTimeline)),
    );
    assert_eq!((ahead.released_segments, ahead.kept_not_played), (0, 2));
    // Частично: 100–300 мс задевает 0–200 и 200–400, но ни один не целиком.
    let partial =
        applied(writer.note_page_removed(LANE, removal(ms(100), PageRemovedRangeEnd::At(ms(300)))));
    assert_eq!(
        (partial.released_segments, partial.kept_partially_removed),
        (0, 2)
    );
    assert_eq!(monitor.held_segment_bytes(), held, "nothing was released");
    // Неизвестная полоса — нарушение порядка, как у других вызовов полосы; и это
    // первая ошибка потока.
    let unknown = LaneId::new(5);
    assert_eq!(
        writer.note_page_removed(unknown, removal(ms(0), PageRemovedRangeEnd::EndOfTimeline)),
        Err(PushedStreamError::UnknownLane { lane: unknown })
    );
    assert_eq!(
        writer.failure(),
        Some(PushedStreamError::UnknownLane { lane: unknown })
    );
}

#[test]
fn page_removal_uses_the_presentation_timeline_after_the_timestamp_offset() {
    let media = split_mp4(VIDEO_H264_64);
    let (mut writer, mut opener) = stream(policy());
    // Смещение +100 с: фрагмент 0–200 мс файла ложится на полку как 100,0–100,2 с,
    // а MSE `remove()` страницы говорит именно этим временем показа.
    writer
        .set_timestamp_offset(LANE, TimestampOffset::from_micros(100_000_000))
        .expect("offset");
    writer.append(LANE, &media.init).expect("init");
    writer.append(LANE, &media.fragments[0]).expect("fragment");
    let mut demuxer = open(&mut opener);
    read_packets(&mut demuxer, 5);
    let held = media.init.len() + media.fragments[0].len();
    assert_eq!(wait_for_held_bytes(&writer, held), held);
    writer.monitor().note_playback_position(ms(100_200));
    // Время файла (до смещения) полке ничего не говорит.
    let raw =
        applied(writer.note_page_removed(LANE, removal(ms(0), PageRemovedRangeEnd::At(ms(200)))));
    assert_eq!(raw, PageRemovalReport::default());
    // Время показа — стирает.
    let shown = applied(writer.note_page_removed(
        LANE,
        removal(ms(100_000), PageRemovedRangeEnd::At(ms(100_200))),
    ));
    assert_eq!(shown.released_segments, 1);
    assert_eq!(writer.monitor().held_segment_bytes(), media.init.len());
}

#[test]
fn page_removal_of_an_mpeg_ts_lane_is_a_no_op() {
    let (mut writer, _opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(LANE, PushedContainer::MpegTs)
        .expect("lane");
    assert_eq!(
        writer.note_page_removed(LANE, removal(ms(0), PageRemovedRangeEnd::EndOfTimeline)),
        Ok(PageRemovalOutcome::UntimedLane)
    );
    assert_eq!(writer.failure(), None);
}

#[test]
fn memory_guard_evicts_old_played_past_below_the_full_threshold_and_keeps_the_behind_window() {
    let media = split_mp4(VIDEO_H264_64_4S);
    let largest = media
        .fragments
        .iter()
        .map(Vec::len)
        .max()
        .expect("fragments");
    // Потолок — init и восемь самых больших фрагментов: порог «полон» (75 %) —
    // около шести. Окно вперёд в секундах (120 с) не наполнится никогда.
    let ceiling = media.init.len() + 8 * largest;
    let window = BufferWindow {
        ahead: Duration::from_secs(120),
        behind: ms(800),
    };
    let small = policy()
        .with_buffer_window(window)
        .expect("window")
        .with_max_held_stream_bytes(NonZeroUsize::new(ceiling).expect("non-zero"));
    let full_at = small.buffer_full_at_held_bytes();
    let (mut writer, mut opener) = stream(small);
    let monitor = writer.monitor();
    writer.append(LANE, &media.init).expect("init");
    let mut demuxer: Option<PushedStreamDemuxer> = None;
    // Плеер играет вровень со страницей: запас вперёд всё время меньше фрагмента.
    for (index, fragment) in media.fragments.iter().enumerate() {
        writer
            .append(LANE, fragment)
            .expect("never the hard ceiling");
        let demuxer = demuxer.get_or_insert_with(|| open(&mut opener));
        read_packets(demuxer, 10);
        let played_to = ms(400) * u32::try_from(index + 1).expect("small");
        monitor.note_playback_position(played_to);
        // Без предохранителя удерживаемое прошлое объявило бы «полон» при пустом
        // запасе вперёд, и честная страница встала бы навсегда.
        assert_eq!(
            monitor.buffer_status().intake,
            BufferIntake::Accepting,
            "after fragment {index}: held {} of full {full_at}",
            monitor.held_segment_bytes()
        );
        assert!(monitor.held_segment_bytes() < full_at);
    }
    let tally = writer.memory_guard_tally();
    assert!(tally.evicted_segments > 0, "the guard had to evict");
    assert_eq!(tally.warnings_logged, 1, "one warning per stream");
    // Позиция 4 с. Окно «назад» (0,8 с) у позиции не тронуто: перемотка на 3,2 с —
    // с полки, без страницы; а вот 0,4 с выселены — туда просим страницу.
    let demuxer = demuxer.as_mut().expect("opened");
    demuxer.seek(ms(3_200)).expect("seek inside behind");
    assert_eq!(monitor.take_page_seek_request(), None);
    demuxer.seek(ms(400)).expect("seek into evicted past");
    assert_eq!(
        monitor.take_page_seek_request(),
        Some(PageSeekRequest { target: ms(400) })
    );
}

#[test]
fn memory_guard_after_a_far_seek_back_evicts_the_old_future_so_the_target_is_not_full() {
    let media = split_mp4(VIDEO_H264_64_4S);
    let held_future = media.init.len() + media.fragments[4..].iter().map(Vec::len).sum::<usize>();
    // Порог «полон» — между «старое будущее» и «оно же + ответ страницы с цели»:
    // до ответа поток не полон, ответ переводит его за порог.
    let threshold = held_future + media.fragments[0].len() / 2;
    let ceiling = threshold.div_ceil(75) * 100;
    // Окно вперёд — 0,8 с: всё, что дальше 0,8 с от цели и не продолжает кусок у
    // неё, — «далёкое будущее».
    let window = BufferWindow {
        ahead: ms(800),
        behind: ms(400),
    };
    let small = policy()
        .with_buffer_window(window)
        .expect("window")
        .with_max_held_stream_bytes(NonZeroUsize::new(ceiling).expect("non-zero"));
    let full_at = small.buffer_full_at_held_bytes();
    assert!(full_at > held_future && full_at < held_future + media.fragments[0].len());
    let (mut writer, mut opener) = stream(small);
    let monitor = writer.monitor();
    writer.append(LANE, &media.init).expect("init");
    // Страница держит 1,6–4,0 с (шесть фрагментов): плеер начал с середины и
    // дочитал их (труба пуста — полка единственная копия).
    for fragment in &media.fragments[4..] {
        writer.append(LANE, fragment).expect("fragment");
    }
    let mut demuxer = open(&mut opener);
    read_packets(&mut demuxer, 60);
    assert_eq!(wait_for_held_bytes(&writer, held_future), held_future);
    monitor.note_playback_position(ms(4_000));
    assert_eq!(writer.memory_guard_tally().evicted_segments, 0);
    // Перемотка назад на 0 вне полки: полка забита старым будущим.
    demuxer.seek(Duration::ZERO).expect("seek back");
    assert_eq!(
        monitor.take_page_seek_request(),
        Some(PageSeekRequest {
            target: Duration::ZERO
        })
    );
    // Страница отвечает с цели; самое далёкое будущее уходит первым, байты — ниже
    // порога, и странице не говорят «полон», пока у цели нет запаса.
    writer.append(LANE, &media.fragments[0]).expect("target");
    assert!(monitor.held_segment_bytes() < full_at);
    assert!(writer.memory_guard_tally().evicted_segments > 0);
    // Самое раннее будущее (1,6 с) держится дольше самого позднего (3,6 с):
    // перемотка на 1,6 с — с полки, на 3,6 с — к странице.
    demuxer.seek(ms(1_600)).expect("seek");
    assert_eq!(monitor.take_page_seek_request(), None);
    demuxer.seek(ms(3_600)).expect("seek");
    assert_eq!(
        monitor.take_page_seek_request(),
        Some(PageSeekRequest { target: ms(3_600) })
    );
}
