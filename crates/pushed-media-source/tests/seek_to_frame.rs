//! Перемотка «до кадра»: по полке, через страницу, перемотка страницей, после
//! конца, байты другого времени до перемотки, живой эфир.
//!
//! Настоящие декодеры видео и звука, настоящее время (обвязка `support/playback`).
//! Фикстуры: `video-h264-64-4s.mp4` (4 с, 25 к/с, десять фрагментов по 0,4 с,
//! каждый с ключевого кадра) и `audio-aac-4s.mp4` (звук к нему, ~0,4 с на
//! фрагмент). Номер фрагмента N — время N × 0,4 с.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::ops::Range;
use std::time::Duration;

use player_core::{PlaybackState, PreparedMedia};
use pushed_media_source::{
    LaneId, PageSeekRequest, PushedContainer, PushedDuration, PushedSourcePolicy,
    PushedStreamMonitor, PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{PlaybackReport, PresentedFrame, open, open_stream, play_controlled};

/// Длительность кадра фикстур (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Длительность фрагмента фикстур.
const FRAGMENT: Duration = Duration::from_millis(400);
/// Длительность роликов.
const CLIP: Duration = Duration::from_secs(4);
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

/// Время начала фрагмента.
fn fragment_start(index: u32) -> Duration {
    FRAGMENT * index
}

/// Страница: шлёт фрагменты обеих полос, как плеер страницы в MSE.
struct Page {
    writer: PushedStreamWriter,
    monitor: PushedStreamMonitor,
    video: FragmentedMp4,
    audio: FragmentedMp4,
    sent_initialization: bool,
}

impl Page {
    /// Поток «видео + звук» с длительностью и страница к нему.
    fn new(duration: PushedDuration) -> (Self, PushedStreamOpener) {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let (mut writer, opener) = pushed_stream(policy).expect("stream");
        writer
            .add_lane(VIDEO, PushedContainer::FragmentedMp4)
            .expect("video lane");
        writer
            .add_lane(AUDIO, PushedContainer::FragmentedMp4)
            .expect("audio lane");
        writer.set_duration(duration);
        let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
        let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
        assert_eq!((video.fragments.len(), audio.fragments.len()), (10, 10));
        let page = Self {
            monitor: writer.monitor(),
            writer,
            video,
            audio,
            sent_initialization: false,
        };
        (page, opener)
    }

    /// Шлёт фрагменты обеих полос (init — перед первым).
    fn push(&mut self, fragments: Range<usize>) {
        if !self.sent_initialization {
            let (video_init, audio_init) = (
                self.video.initialization.clone(),
                self.audio.initialization.clone(),
            );
            self.writer.append(VIDEO, &video_init).expect("video init");
            self.writer.append(AUDIO, &audio_init).expect("audio init");
            self.sent_initialization = true;
        }
        for index in fragments {
            let (video, audio) = (
                self.video.fragments[index].clone(),
                self.audio.fragments[index].clone(),
            );
            self.writer.append(VIDEO, &video).expect("video fragment");
            self.writer.append(AUDIO, &audio).expect("audio fragment");
        }
    }

    /// Шлёт фрагменты и конец потока.
    fn push_to_end(&mut self, fragments: Range<usize>) {
        self.push(fragments);
        self.writer.end_of_stream().expect("end of stream");
    }
}

/// Кадры, впервые показанные после команды перемотки `seek_index`.
fn frames_after_seek(report: &PlaybackReport, seek_index: usize) -> Vec<PresentedFrame> {
    let (seek_tick, _) = report.seeks[seek_index];
    report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .copied()
        .collect()
}

/// Общие утверждения про посадку: первый кадр после перемотки — цель ± кадр,
/// ни одного кадра старого времени (раньше цели − кадр), дальше — строго по
/// порядку и до конца ролика, звук идёт вровень с видео.
fn assert_landed_and_played_through(report: &PlaybackReport, seek_index: usize, target: Duration) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let after = frames_after_seek(report, seek_index);
    let first = after.first().expect("frames after the seek");
    assert!(
        first.pts.abs_diff(target) <= FRAME,
        "first frame after the seek at {:?}, target {target:?}",
        first.pts
    );
    assert!(
        after.iter().all(|frame| frame.pts + FRAME >= target),
        "a frame from before the target was shown after the seek: {:?}",
        after.iter().map(|frame| frame.pts).collect::<Vec<_>>()
    );
    assert!(
        after.windows(2).all(|pair| pair[0].pts < pair[1].pts),
        "frames after the seek are not in order"
    );
    assert_eq!(
        after.last().map(|frame| frame.pts),
        Some(CLIP - FRAME),
        "playback after the seek reaches the end"
    );
    // Кадры идут подряд: пропуск больше одного кадра — потерянное время.
    assert!(
        after
            .windows(2)
            .all(|pair| pair[1].pts - pair[0].pts <= FRAME * 2),
        "frames after the seek skip time"
    );
    // Звук: позиция плеера (часы звука) при показе кадра — вровень с кадром.
    let worst = after
        .iter()
        .skip(1)
        .map(|frame| frame.pts.abs_diff(frame.position))
        .max()
        .unwrap_or_default();
    assert!(worst <= FRAME, "A/V offset after the seek {worst:?}");
}

/// Собирает все просьбы странице за прогон.
fn take_requests(monitor: &PushedStreamMonitor, requests: &mut Vec<PageSeekRequest>) {
    requests.extend(monitor.take_page_seek_request());
}

fn prepared(demuxer: pushed_media_source::PushedStreamDemuxer) -> PreparedMedia {
    PreparedMedia::from_external_label("pushed media seek test", Box::new(demuxer))
}

#[test]
fn seek_inside_the_shelf_lands_on_target_without_asking_the_page() {
    let (mut page, mut opener) = Page::new(PushedDuration::Finite(CLIP));
    page.push_to_end(0..10);
    let target = fragment_start(6) + FRAME * 3;
    let monitor = page.monitor.clone();
    let mut requests = Vec::new();
    let mut seek_position = None;
    let report = play_controlled(prepared(open(&mut opener)), |control| {
        take_requests(&monitor, &mut requests);
        if seek_position.is_none() && control.observation().position >= FRAGMENT {
            seek_position = Some(control.observation().position);
            control.seek(target);
        }
    });
    assert_landed_and_played_through(&report, 0, target);
    assert_eq!(requests, [], "an on-shelf seek must not contact the page");
    // Звук продолжился: до перемотки + от цели до конца (с запасом на старт).
    let played_before = seek_position.expect("seeked");
    assert!(
        report.audio_duration() + Duration::from_millis(300) >= played_before + (CLIP - target),
        "audio {:?}",
        report.audio_duration()
    );
    assert!(report.audio_rms() > 0.05);
}

#[test]
fn seek_outside_the_shelf_asks_the_page_once_and_ignores_its_stale_bytes() {
    let (mut page, mut opener) = Page::new(PushedDuration::Finite(CLIP));
    page.push(0..3);
    let target = fragment_start(7);
    let monitor = page.monitor.clone();
    let mut requests = Vec::new();
    let mut seek_tick = None;
    let report = play_controlled(prepared(open(&mut opener)), |control| {
        take_requests(&monitor, &mut requests);
        let tick = control.observation().tick;
        match seek_tick {
            None if control.observation().position >= FRAGMENT => {
                seek_tick = Some(tick);
                control.seek(target);
            }
            // Страница ещё досылает то, что качала до перемотки: это время
            // раньше цели и на экран попасть не должно.
            Some(at) if tick == at + 10 => page.push(3..5),
            // Страница перемоталась и шлёт с цели.
            Some(at) if tick == at + 40 => page.push_to_end(7..10),
            _ => {}
        }
    });
    assert_eq!(
        requests,
        [PageSeekRequest { target }],
        "exactly one request"
    );
    // Пока страница не прислала байты с цели, плеер стоит в перемотке.
    assert!(
        report.waiting_ticks >= 30,
        "waited {} ticks",
        report.waiting_ticks
    );
    assert_landed_and_played_through(&report, 0, target);
}

#[test]
fn quick_second_seek_supersedes_the_first_request() {
    let (mut page, mut opener) = Page::new(PushedDuration::Finite(CLIP));
    page.push(0..3);
    let (first, latest) = (fragment_start(5), fragment_start(8));
    let monitor = page.monitor.clone();
    let mut requests = Vec::new();
    let mut seek_tick = None;
    let report = play_controlled(prepared(open(&mut opener)), |control| {
        let tick = control.observation().tick;
        match seek_tick {
            None if control.observation().position >= FRAGMENT => {
                seek_tick = Some(tick);
                control.seek(first);
                control.seek(latest);
            }
            Some(at) if tick == at + 20 => page.push_to_end(8..10),
            _ => {}
        }
        // Приложение забирает просьбы после команд: старая уже заменена.
        take_requests(&monitor, &mut requests);
    });
    assert_eq!(requests, [PageSeekRequest { target: latest }]);
    assert_landed_and_played_through(&report, 1, latest);
}

#[test]
fn page_initiated_seek_plays_from_its_target_without_a_request() {
    let (mut page, mut opener) = Page::new(PushedDuration::Finite(CLIP));
    page.push(0..3);
    let target = fragment_start(6);
    let monitor = page.monitor.clone();
    let mut requests = Vec::new();
    let mut seek_tick = None;
    let report = play_controlled(prepared(open(&mut opener)), |control| {
        let tick = control.observation().tick;
        match seek_tick {
            None if control.observation().position >= FRAGMENT => {
                seek_tick = Some(tick);
                // `seek_started{page_seek}`: приложение отмечает цель страницы и
                // командует плееру туда же.
                page.writer.note_page_seek(target);
                control.seek(target);
            }
            Some(at) if tick == at + 20 => page.push_to_end(6..10),
            _ => {}
        }
        take_requests(&monitor, &mut requests);
    });
    assert_eq!(requests, [], "the page is already seeking there");
    assert_landed_and_played_through(&report, 0, target);
}

#[test]
fn seek_back_after_the_end_plays_again_from_the_shelf() {
    let (mut page, mut opener) = Page::new(PushedDuration::Finite(CLIP));
    page.push_to_end(0..10);
    let target = fragment_start(8);
    let monitor = page.monitor.clone();
    let mut requests = Vec::new();
    let mut seeked = false;
    let report = play_controlled(prepared(open(&mut opener)), |control| {
        take_requests(&monitor, &mut requests);
        if !seeked && control.state() == PlaybackState::Ended {
            seeked = true;
            // После конца плеер садится на паузу; «играть» — как кнопка.
            control.seek(target);
            control.play();
        }
    });
    assert!(seeked, "the player reached the end first");
    assert_eq!(requests, []);
    assert_landed_and_played_through(&report, 0, target);
}

#[test]
fn seek_to_the_end_after_end_of_stream_finishes_instead_of_waiting() {
    // На длительность и в середину последнего кадра (−½ кадра): страница уже
    // сказала «байтов больше нет» — плеер доигрывает и заканчивает, страницу
    // не просят, ожидания (крутилки) без конца нет.
    for target in [CLIP, CLIP - FRAME / 2] {
        let (mut page, mut opener) = Page::new(PushedDuration::Finite(CLIP));
        page.push_to_end(0..10);
        let monitor = page.monitor.clone();
        let mut requests = Vec::new();
        let mut seek_tick = None;
        let report = play_controlled(prepared(open(&mut opener)), |control| {
            take_requests(&monitor, &mut requests);
            if seek_tick.is_none() && control.observation().position >= FRAGMENT {
                seek_tick = Some(control.observation().tick);
                control.seek(target);
            }
        });
        assert!(report.errors.is_empty(), "{target:?}: {:?}", report.errors);
        assert_eq!(report.final_state, PlaybackState::Ended, "{target:?}");
        assert_eq!(
            requests,
            [],
            "{target:?}: the page has nothing more to send"
        );
        // После перемотки — только хвост последнего фрагмента (источник сел на
        // него); каким кадром хвоста плеер встречает конец — дело плеера.
        let after = frames_after_seek(&report, 0);
        assert!(
            after.iter().all(|frame| frame.pts >= fragment_start(9)),
            "{target:?}: {:?}",
            after.iter().map(|frame| frame.pts).collect::<Vec<_>>()
        );
        let (_, last_state, _) = report.state_trace.last().copied().expect("trace");
        assert_eq!(last_state, PlaybackState::Ended);
        let finished_at = report.state_trace.len();
        let seeked_at = seek_tick.expect("seeked");
        assert!(
            finished_at - seeked_at < 400,
            "{target:?}: took {} ticks to finish",
            finished_at - seeked_at
        );
    }
}

#[test]
fn seek_outside_the_shelf_to_a_mid_fragment_target_hides_the_fragment_start() {
    // Цели нет на полке: плеер получает ответ «пакеты с цели», а страница потом
    // присылает фрагмент, который начинается РАНЬШЕ цели (2,8 с при цели 3,0 с).
    // Плеер декодирует их как разгон и прячет всё до цели (Accurate).
    let (mut page, mut opener) = Page::new(PushedDuration::Finite(CLIP));
    page.push(0..3);
    let target = fragment_start(7) + FRAME * 5;
    let monitor = page.monitor.clone();
    let mut requests = Vec::new();
    let mut seek_tick = None;
    let report = play_controlled(prepared(open(&mut opener)), |control| {
        take_requests(&monitor, &mut requests);
        let tick = control.observation().tick;
        match seek_tick {
            None if control.observation().position >= FRAGMENT => {
                seek_tick = Some(tick);
                control.seek(target);
            }
            Some(at) if tick == at + 20 => page.push_to_end(7..10),
            _ => {}
        }
    });
    assert_eq!(requests, [PageSeekRequest { target }]);
    assert_landed_and_played_through(&report, 0, target);
}

#[test]
fn bytes_of_another_time_that_arrived_before_the_seek_play_instantly() {
    // Twitch держит MSE в воркере: страница уже прыгнула и шлёт байты другого
    // времени, а `seek_started` придёт позже. Эти байты не продолжают поданное —
    // лежат на полке и не играются; перемотка к ним — сразу с полки.
    let (mut page, mut opener) = Page::new(PushedDuration::Finite(CLIP));
    page.push(0..3);
    let target = fragment_start(6);
    let monitor = page.monitor.clone();
    let mut requests = Vec::new();
    let mut pushed_other_time = false;
    let mut seek_tick = None;
    let report = play_controlled(prepared(open(&mut opener)), |control| {
        take_requests(&monitor, &mut requests);
        let position = control.observation().position;
        if !pushed_other_time && position >= FRAME * 3 {
            pushed_other_time = true;
            page.push_to_end(6..10);
        }
        if seek_tick.is_none() && position >= FRAGMENT {
            seek_tick = Some(control.observation().tick);
            control.seek(target);
        }
    });
    let (seek_tick, _) = report.seeks[0];
    assert!(
        report
            .frames
            .iter()
            .filter(|frame| frame.tick <= seek_tick)
            .all(|frame| frame.pts < fragment_start(3)),
        "bytes of another time played before the seek"
    );
    assert_eq!(requests, [], "the target was on the shelf");
    assert_landed_and_played_through(&report, 0, target);
}

#[test]
fn unknown_duration_at_open_keeps_the_stream_unseekable() {
    // Ограничение: `player-core` читает длительность и перематываемость один раз —
    // когда приложение собирает `PreparedMedia`. Длительность, пришедшая после
    // этого, перемотку не включает: плеер отказывает в перемотке (ошибка «нельзя
    // перематывать») и играет дальше.
    let (mut page, mut opener) = Page::new(PushedDuration::Unknown);
    page.push(0..4);
    let prepared = prepared(open(&mut opener));
    page.writer.set_duration(PushedDuration::Finite(CLIP));
    page.push_to_end(4..10);
    let mut seeked = false;
    let report = play_controlled(prepared, |control| {
        if !seeked && control.observation().position >= FRAGMENT {
            seeked = true;
            control.seek(fragment_start(8));
        }
    });
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_eq!(report.errors.len(), 1, "errors: {:?}", report.errors);
    assert!(
        report.errors[0].contains("SeekUnavailable"),
        "{:?}",
        report.errors
    );
    // Ни одного кадра пропущено: перемотки не было.
    let times: Vec<Duration> = report.frames.iter().map(|frame| frame.pts).collect();
    assert_eq!(
        times,
        (0..100).map(|index| FRAME * index).collect::<Vec<_>>()
    );
}

#[test]
fn live_stream_publishes_its_shelf_as_a_dynamic_timeline_and_seeks_inside_it() {
    let (mut page, mut opener) = Page::new(PushedDuration::Live);
    page.push(0..5);
    let opened = open_stream(&mut opener);
    let port = opened
        .live_timeline
        .expect("live streams get a dynamic timeline");
    assert_eq!(media_core::Demuxer::duration(&opened.demuxer), None);
    let player_ms =
        |milliseconds| media_core::MediaTime::from_duration(Duration::from_millis(milliseconds));
    let state = port.observe().snapshot.state;
    assert_eq!(state.live_edge(), player_ms(2_000));
    assert_eq!(state.join_position(), player_ms(0));
    let range = state.seekable_range().expect("dvr range");
    assert_eq!((range.start, range.end), (player_ms(0), player_ms(2_000)));
    // Новые сегменты двигают край эфира.
    page.push(5..7);
    assert_eq!(port.observe().snapshot.state.live_edge(), player_ms(2_800));

    let prepared = prepared(opened.demuxer)
        .with_dynamic_timeline(port)
        .expect("live timeline accepted");
    let target = fragment_start(5);
    let monitor = page.monitor.clone();
    let mut requests = Vec::new();
    let mut seek_tick = None;
    let report = play_controlled(prepared, |control| {
        take_requests(&monitor, &mut requests);
        monitor.note_playback_position(control.observation().position);
        let tick = control.observation().tick;
        match seek_tick {
            None if control.observation().position >= FRAGMENT => {
                seek_tick = Some(tick);
                control.seek(target);
            }
            Some(at) if tick == at + 5 => page.push_to_end(7..10),
            _ => {}
        }
    });
    assert_eq!(requests, [], "the target was inside the DVR range");
    assert_landed_and_played_through(&report, 0, target);
}
