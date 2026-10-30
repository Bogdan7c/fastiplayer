//! Полка «до кадра»: порядок прихода, обратное давление и байты после конца потока.
//!
//! Настоящие декодеры видео и звука, настоящее время (обвязка `support/playback`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use player_core::PlaybackState;
use pushed_media_source::{
    BufferIntake, BufferWindow, LaneId, PushedContainer, PushedSourcePolicy, PushedStreamMonitor,
    PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{PlaybackReport, TickObservation, open, play, play_observed};

/// Длительность кадра фикстур (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

fn frame_times(range: std::ops::Range<u32>) -> Vec<Duration> {
    range.map(|index| FRAME * index).collect()
}

fn presented_times(report: &PlaybackReport) -> Vec<Duration> {
    report.frames.iter().map(|frame| frame.pts).collect()
}

fn assert_clean_end(report: &PlaybackReport) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
}

/// Поток «видео + звук» с политикой.
fn av_stream(policy: PushedSourcePolicy) -> (PushedStreamWriter, PushedStreamOpener) {
    let (mut writer, opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    (writer, opener)
}

fn recommended() -> PushedSourcePolicy {
    PushedSourcePolicy::recommended().expect("policy")
}

#[test]
fn fragments_arriving_out_of_order_play_every_frame_once_in_time_order() {
    let video = FragmentedMp4::split(&own_fixture("video-h264-64.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac.mp4"));
    let (mut writer, mut opener) = av_stream(recommended());
    // Видео: 0, 2, 1, 3. Звук: первый, затем остальные задом наперёд. Полка
    // держит пришедшее раньше времени и подаёт его, когда дыра закрылась.
    writer.append(VIDEO, &video.stream_of(0..1)).expect("0");
    for index in [2, 1, 3] {
        writer
            .append(VIDEO, &video.fragments[index])
            .expect("fragment");
    }
    writer
        .append(AUDIO, &audio.stream_of(0..1))
        .expect("audio 0");
    for fragment in audio.fragments[1..].iter().rev() {
        writer.append(AUDIO, fragment).expect("audio fragment");
    }
    writer.end_of_stream().expect("end of stream");

    let report = play(open(&mut opener), |_| {});
    assert_clean_end(&report);
    assert_eq!(presented_times(&report), frame_times(0..20));
    assert!(report.audio_duration() >= Duration::from_millis(780));
    assert!(
        report.audio_rms() > 0.05,
        "audio RMS {}",
        report.audio_rms()
    );
}

/// Страница, которая слушает «полон»: шлёт следующий фрагмент каждой полосы,
/// пока источник принимает, и сообщает ему позицию плеера, как приложение.
struct PolitePage {
    writer: PushedStreamWriter,
    monitor: PushedStreamMonitor,
    video: FragmentedMp4,
    audio: FragmentedMp4,
    /// Сколько фрагментов каждой полосы уже отправлено.
    sent: usize,
    ended: bool,
    /// Наблюдения: был ли «полон», наибольшие удерживаемые байты.
    saw_full: bool,
    max_held: usize,
}

impl PolitePage {
    /// Время конца отправленного (фрагменты по 0,4 с).
    fn pushed_until(&self) -> Duration {
        Duration::from_millis(400) * u32::try_from(self.sent).expect("small")
    }

    fn push_while_accepting(&mut self) {
        while self.sent < self.video.fragments.len() {
            let status = self.monitor.buffer_status();
            if status.intake == BufferIntake::Full {
                self.saw_full = true;
                break;
            }
            let index = self.sent;
            if index == 0 {
                let (video_init, audio_init) = (
                    self.video.initialization.clone(),
                    self.audio.initialization.clone(),
                );
                self.writer.append(VIDEO, &video_init).expect("video init");
                self.writer.append(AUDIO, &audio_init).expect("audio init");
            }
            self.writer
                .append(VIDEO, &self.video.fragments[index])
                .expect("video fragment");
            self.writer
                .append(AUDIO, &self.audio.fragments[index])
                .expect("audio fragment");
            self.sent += 1;
        }
        if self.sent == self.video.fragments.len() && !self.ended {
            self.writer.end_of_stream().expect("end of stream");
            self.ended = true;
        }
        self.max_held = self.max_held.max(self.monitor.held_segment_bytes());
    }

    fn observe(&mut self, observation: TickObservation, window: BufferWindow) {
        self.monitor.note_playback_position(observation.position);
        let status = self.monitor.buffer_status();
        // Запас вперёд не убегает дальше отправленного и окна (+ один фрагмент,
        // отправленный, пока запас был чуть меньше окна).
        let pushed_ahead = self.pushed_until().saturating_sub(observation.position);
        assert!(
            status.buffered_ahead <= pushed_ahead + Duration::from_millis(1),
            "ahead {:?} at {:?}, pushed until {:?}",
            status.buffered_ahead,
            observation.position,
            self.pushed_until()
        );
        assert!(status.buffered_ahead <= window.ahead + Duration::from_millis(400));
        self.push_while_accepting();
    }
}

#[test]
fn backpressure_keeps_the_shelf_within_the_window_while_playing() {
    let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
    assert_eq!(video.fragments.len(), 10);
    assert_eq!(audio.fragments.len(), 10);
    let window = BufferWindow {
        ahead: Duration::from_secs(1),
        behind: Duration::from_millis(400),
    };
    let (writer, mut opener) = av_stream(recommended().with_buffer_window(window).expect("window"));
    let monitor = writer.monitor();
    let fragment_bytes = |index: usize| video.fragments[index].len() + audio.fragments[index].len();
    let total: usize = (0..10).map(fragment_bytes).sum();
    let largest = (0..10).map(fragment_bytes).max().expect("fragments");
    let initializations = video.initialization.len() + audio.initialization.len();
    let mut page = PolitePage {
        writer,
        monitor: monitor.clone(),
        video,
        audio,
        sent: 0,
        ended: false,
        saw_full: false,
        max_held: 0,
    };
    // До старта страница шлёт, пока не услышит «полон»: 3 фрагмента по 0,4 с.
    page.push_while_accepting();
    assert!(page.saw_full, "the source never reported full");
    assert_eq!(page.sent, 3);

    let report = play_observed(open(&mut opener), |observation| {
        page.observe(observation, window);
    });
    assert_clean_end(&report);
    assert_eq!(presented_times(&report), frame_times(0..100));
    assert!(page.ended, "the page sent everything");
    // Полка — это окно, а не весь ролик: позади ≤ 0,4 с + фрагмент, впереди
    // < 1 с + фрагмент, т. е. не больше 7 фрагментов из 10.
    assert!(
        page.max_held <= initializations + 7 * largest && page.max_held < initializations + total,
        "held {} of {} bytes",
        page.max_held,
        initializations + total
    );
    // Позиция далеко за концом: всё поданное позади окна выброшено, учёт
    // возвращается ровно к init-сегментам полос (их держат сами полосы).
    monitor.note_playback_position(Duration::from_secs(10));
    assert_eq!(monitor.held_segment_bytes(), initializations);
}

#[test]
fn bytes_after_end_of_stream_before_the_player_reaches_it_play_through() {
    let video = FragmentedMp4::split(&own_fixture("video-h264-64.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac.mp4"));
    let (mut writer, mut opener) = av_stream(recommended());
    writer
        .append(VIDEO, &video.stream_of(0..2))
        .expect("first half");
    writer.append(AUDIO, &audio.stream_of(0..4)).expect("audio");
    writer.end_of_stream().expect("end of stream");
    // MSE: `appendBuffer` после `endOfStream` снова открывает поток.
    writer
        .append(VIDEO, &video.fragments[2..].concat())
        .expect("continuation reopens input");
    writer.end_of_stream().expect("end again");

    let report = play(open(&mut opener), |_| {});
    assert_clean_end(&report);
    assert_eq!(presented_times(&report), frame_times(0..20));
}

/// Продолжение пришло, когда плеер уже мог получить конец потока.
///
/// Что проверяется и почему: если демуксер полосы уже отдал плееру
/// `EndOfStream`, плеер доигрывает и заканчивает — продолжение ему не
/// показывается (это решает перемотка пакета F: страница перематывает назад, и
/// плеер начинает с полки). Если не успел — продолжение играет. Оба исхода честны,
/// поэтому утверждается общее: без ошибок, до конца, первая половина целиком,
/// каждый показанный кадр ровно раз и по порядку.
#[test]
fn bytes_after_end_of_stream_during_playback_never_error() {
    let video = FragmentedMp4::split(&own_fixture("video-h264-64.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac.mp4"));
    let (mut writer, mut opener) = av_stream(recommended());
    writer
        .append(VIDEO, &video.stream_of(0..2))
        .expect("first half");
    writer.append(AUDIO, &audio.stream_of(0..4)).expect("audio");
    writer.end_of_stream().expect("end of stream");
    let mut continuation = Some(video.fragments[2..].concat());
    let report = play(open(&mut opener), |tick| {
        if tick == 40
            && let Some(bytes) = continuation.take()
        {
            writer.append(VIDEO, &bytes).expect("continuation accepted");
            writer.end_of_stream().expect("end again");
        }
    });
    assert_clean_end(&report);
    assert_eq!(writer.failure(), None);
    let times = presented_times(&report);
    assert!(times.len() >= 10, "first half shown: {times:?}");
    assert_eq!(
        times,
        frame_times(0..u32::try_from(times.len()).expect("small"))
    );
}
