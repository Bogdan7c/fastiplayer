//! Поток, поставленный в плеер сразу на позицию страницы (сессия 03e моста), — до кадра.
//!
//! Так процесс браузерного режима ставит новое поколение (новый `MediaSource`): страница
//! сказала «я на T» (`note_page_seek`), демуксер потока синхронно перематывается на T, и
//! `PreparedMedia` несёт готовую позицию (`PreparedInitialPosition::PositionedAt`) —
//! плеер начинает с T, без «с нуля, потом перемотка».
//!
//! Фикстуры: `video-h264-64-4s.mp4` (4 с, 25 к/с, десять фрагментов по 0,4 с, каждый
//! с ключевого кадра) и `audio-aac-4s.mp4`. Настоящие декодеры, настоящее время.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::ops::Range;
use std::time::Duration;

use media_core::{DemuxSeekRequest, DemuxSeekability, Demuxer, MediaTime};
use player_core::{
    PlaybackState, PreparedDemuxSeekLandingPolicy, PreparedInitialPosition, PreparedMedia,
};
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamMonitor,
    PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{PlaybackReport, open_stream, play_controlled};

/// Длительность кадра фикстур (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Длительность ролика.
const CLIP: Duration = Duration::from_secs(4);
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);
/// Шаг плеера, на котором «страница» досылает байты (≈ 0,5 с настоящего времени).
const LATE_BYTES_TICK: usize = 100;

/// Страница: кладёт фрагменты обеих полос, как плеер страницы в MSE.
struct Page {
    writer: PushedStreamWriter,
    monitor: PushedStreamMonitor,
    video: FragmentedMp4,
    audio: FragmentedMp4,
}

impl Page {
    /// Поток «видео + звук»; `duration` — что страница сказала о длительности.
    fn new(duration: Option<PushedDuration>) -> (Self, PushedStreamOpener) {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let (mut writer, opener) = pushed_stream(policy).expect("stream");
        writer
            .add_lane(VIDEO, PushedContainer::FragmentedMp4)
            .expect("video lane");
        writer
            .add_lane(AUDIO, PushedContainer::FragmentedMp4)
            .expect("audio lane");
        if let Some(duration) = duration {
            writer.set_duration(duration);
        }
        let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
        let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
        writer
            .append(VIDEO, &video.initialization)
            .expect("video init");
        writer
            .append(AUDIO, &audio.initialization)
            .expect("audio init");
        let page = Self {
            monitor: writer.monitor(),
            writer,
            video,
            audio,
        };
        (page, opener)
    }

    /// Кладёт фрагменты обеих полос (номер N — время N × 0,4 с).
    fn push(&mut self, fragments: Range<usize>) {
        for index in fragments {
            let (video, audio) = (
                self.video.fragments[index].clone(),
                self.audio.fragments[index].clone(),
            );
            self.writer.append(VIDEO, &video).expect("video fragment");
            self.writer.append(AUDIO, &audio).expect("audio fragment");
        }
    }

    /// Кладёт фрагменты и конец потока.
    fn push_to_end(&mut self, fragments: Range<usize>) {
        self.push(fragments);
        self.writer.end_of_stream().expect("end of stream");
    }
}

/// Открывает поток и ставит его на `target`, как процесс браузерного режима
/// (`app-egui` `generation_start::position_segments_demuxer`).
fn open_at(opener: &mut PushedStreamOpener, target: Duration) -> PreparedMedia {
    let mut demuxer = open_stream(opener).demuxer;
    let result = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(target))
        .expect("the stream seeks without waiting");
    let target_position = MediaTime::from_duration(target);
    assert!(
        result.actual_position <= target_position,
        "landing {:?} after the target {target:?}",
        result.actual_position
    );
    PreparedMedia::from_external_label("pushed media test", Box::new(demuxer))
        .with_prepared_initial_position(PreparedInitialPosition::PositionedAt {
            target_position,
            result,
            landing_policy: PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        })
        .expect("a landing before the target is a valid start position")
}

/// Плеер начал с позиции страницы и доиграл: первый кадр — цель ± кадр, ни одного
/// кадра раньше, дальше по порядку до конца ролика, звук идёт с цели до конца.
fn assert_started_at(report: &PlaybackReport, target: Duration) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let first = report.frames.first().expect("frames were shown");
    assert!(
        first.pts.abs_diff(target) <= FRAME,
        "first frame at {:?}, page position {target:?}",
        first.pts
    );
    assert!(
        report
            .frames
            .iter()
            .all(|frame| frame.pts + FRAME >= target),
        "a frame from before the page position was shown: {:?}",
        report
            .frames
            .iter()
            .map(|frame| frame.pts)
            .collect::<Vec<_>>()
    );
    assert!(
        report
            .frames
            .windows(2)
            .all(|pair| pair[0].pts < pair[1].pts),
        "frames are not in order"
    );
    let last = report.frames.last().expect("frames");
    assert!(
        last.pts + FRAME >= CLIP - FRAME,
        "stopped at {:?}",
        last.pts
    );
    // Звук — от цели до конца (а не весь ролик с нуля и не обрывок).
    let audio = report.audio_duration();
    let expected_audio = CLIP - target;
    assert!(
        audio.abs_diff(expected_audio) <= Duration::from_millis(150),
        "audio {audio:?}, expected about {expected_audio:?}"
    );
}

/// Позиция страницы лежит на полке: плеер стартует с неё сразу.
#[test]
fn a_stream_installed_at_the_page_position_starts_there() {
    let (mut page, mut opener) = Page::new(Some(PushedDuration::Finite(CLIP)));
    page.push_to_end(0..10);
    let target = Duration::from_millis(2_100);
    page.writer.note_page_seek(target);
    let prepared = open_at(&mut opener, target);

    let report = play_controlled(prepared, |_control| {});

    assert_started_at(&report, target);
    assert_eq!(page.monitor.take_page_seek_request(), None);
}

/// Позиции страницы ещё нет на полке, когда поколение открылось (страница сначала
/// сказала «я на T», а коробки с T положит потом): плеер ждёт её байтов — не
/// падает, не играет то, что лежит раньше, и не просит страницу перемотаться (она
/// уже идёт туда сама).
#[test]
fn a_page_position_beyond_the_shelf_waits_for_the_page_bytes() {
    let (mut page, mut opener) = Page::new(Some(PushedDuration::Finite(CLIP)));
    page.push(0..3);
    let target = Duration::from_millis(2_500);
    page.writer.note_page_seek(target);
    let prepared = open_at(&mut opener, target);

    let mut late_bytes_sent = false;
    let report = play_controlled(prepared, |control| {
        if !late_bytes_sent && control.observation().tick >= LATE_BYTES_TICK {
            page.push_to_end(6..10);
            late_bytes_sent = true;
        }
    });

    assert!(
        late_bytes_sent,
        "the player finished before the page bytes came"
    );
    assert_started_at(&report, target);
    assert_eq!(page.monitor.take_page_seek_request(), None);
}

/// Страница ещё не сказала длительность (`duration_changed` придёт позже): поток
/// объявляет себя неперематываемым, а ядро ставит медиа «сразу на позицию» только
/// перематываемым. Поэтому процесс браузерного режима такое поколение не
/// позиционирует (`generation_start`), а ставит с начала — ограничение 03b
/// «длительность после открытия не подхватывается». Когда его снимут, этот тест
/// подскажет, что запрет в процессе можно убрать.
#[test]
fn a_stream_without_a_known_duration_is_not_seekable_and_so_is_not_positioned() {
    let (mut page, mut opener) = Page::new(None);
    page.push_to_end(0..10);
    let demuxer = open_stream(&mut opener).demuxer;
    assert!(
        matches!(demuxer.seekability(), DemuxSeekability::NotSeekable { .. }),
        "{:?}",
        demuxer.seekability()
    );
}
