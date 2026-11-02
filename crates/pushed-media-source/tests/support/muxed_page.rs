//! Страница с ОДНИМ SourceBuffer «видео + звук» (сессия 09a моста) и проверки «до кадра».
//!
//! Общая обвязка тестов muxed-полосы: поток с одной полосой fMP4, открытие с начала
//! или сразу на позиции страницы (как процесс браузерного режима ставит новое
//! поколение, `generation_start`) и утверждения про показанные кадры.

use std::time::Duration;

use media_core::{DemuxSeekRequest, Demuxer, MediaTime};
use player_core::{
    PlaybackState, PreparedDemuxSeekLandingPolicy, PreparedInitialPosition, PreparedMedia,
};
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamMonitor,
    PushedStreamOpener, PushedStreamWriter, pushed_stream,
};

use super::media::{FragmentedMp4, own_fixture};
use super::playback::{PlaybackReport, PresentedFrame, open, open_stream};

/// Длительность кадра фикстур (25 к/с).
pub const FRAME: Duration = Duration::from_millis(40);
/// Единственная полоса страницы: и видео, и звук.
pub const MUXED: LaneId = LaneId::new(0);

/// Страница с одним SourceBuffer `video/mp4; codecs="avc1…, mp4a…"`.
pub struct MuxedPage {
    /// Писатель потока (то, что делает приложение по сообщениям страницы).
    pub writer: PushedStreamWriter,
    /// Монитор: просьбы странице, взгляд на полку.
    pub monitor: PushedStreamMonitor,
    /// Фикстура, разрезанная на init и фрагменты.
    pub media: FragmentedMp4,
}

impl MuxedPage {
    /// Поток с одной полосой из фикстуры `fixture` длиной `clip`; init уже положен.
    pub fn new(fixture: &str, clip: Duration) -> (Self, PushedStreamOpener) {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let (mut writer, opener) = pushed_stream(policy).expect("stream");
        writer
            .add_lane(MUXED, PushedContainer::FragmentedMp4)
            .expect("muxed lane");
        writer.set_duration(PushedDuration::Finite(clip));
        let media = FragmentedMp4::split(&own_fixture(fixture));
        writer
            .append(MUXED, &media.initialization)
            .expect("muxed init");
        let page = Self {
            monitor: writer.monitor(),
            writer,
            media,
        };
        (page, opener)
    }

    /// Кладёт все фрагменты фикстуры и конец потока.
    pub fn push_everything(&mut self) {
        let fragments = self.media.fragments.clone();
        self.push_and_end(&fragments);
    }

    /// Кладёт `fragments` по порядку и конец потока.
    pub fn push_and_end(&mut self, fragments: &[Vec<u8>]) {
        for fragment in fragments {
            self.writer.append(MUXED, fragment).expect("muxed fragment");
        }
        self.writer.end_of_stream().expect("end of stream");
    }
}

/// Открывает поток с начала.
pub fn open_from_start(opener: &mut PushedStreamOpener) -> PreparedMedia {
    PreparedMedia::from_external_label("pushed muxed test", Box::new(open(opener)))
}

/// Открывает поток и ставит его на `target`, как процесс браузерного режима ставит
/// новое поколение на позицию страницы.
pub fn open_at(opener: &mut PushedStreamOpener, target: Duration) -> PreparedMedia {
    let mut demuxer = open_stream(opener).demuxer;
    let result = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(target))
        .expect("the stream seeks without waiting");
    let target_position = MediaTime::from_duration(target);
    PreparedMedia::from_external_label("pushed muxed test", Box::new(demuxer))
        .with_prepared_initial_position(PreparedInitialPosition::PositionedAt {
            target_position,
            result,
            landing_policy: PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        })
        .expect("a landing before the target is a valid start position")
}

/// Плеер доиграл без ошибок.
pub fn assert_ended_cleanly(report: &PlaybackReport) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
}

/// Кадры идут строго по порядку, без пропусков больше кадра, до кадра `last_frame`.
pub fn assert_frames_run_to(frames: &[PresentedFrame], last_frame: Duration) {
    let times = || frames.iter().map(|frame| frame.pts).collect::<Vec<_>>();
    assert!(
        frames.windows(2).all(|pair| pair[0].pts < pair[1].pts),
        "frames are not in order: {:?}",
        times()
    );
    assert!(
        frames
            .windows(2)
            .all(|pair| pair[1].pts - pair[0].pts <= FRAME * 2),
        "frames skip time: {:?}",
        times()
    );
    let last = frames.last().expect("frames were shown");
    assert!(
        last.pts.abs_diff(last_frame) <= Duration::from_millis(1),
        "stopped at {:?}",
        last.pts
    );
}

/// Первый показанный кадр — цель ± кадр, раньше цели ничего не показано.
pub fn assert_starts_at(frames: &[PresentedFrame], target: Duration) {
    let first = frames.first().expect("frames were shown");
    assert!(
        first.pts.abs_diff(target) <= FRAME,
        "first frame at {:?}, target {target:?}",
        first.pts
    );
    assert!(
        frames.iter().all(|frame| frame.pts + FRAME >= target),
        "a frame from before the target was shown: {:?}",
        frames.iter().map(|frame| frame.pts).collect::<Vec<_>>()
    );
}

/// Кадры, впервые показанные до первой команды перемотки (включая её шаг).
pub fn frames_before_first_seek(report: &PlaybackReport) -> Vec<PresentedFrame> {
    let (seek_tick, _) = report.seeks[0];
    report
        .frames
        .iter()
        .filter(|frame| frame.tick <= seek_tick)
        .copied()
        .collect()
}

/// Кадры, впервые показанные после первой команды перемотки.
pub fn frames_after_first_seek(report: &PlaybackReport) -> Vec<PresentedFrame> {
    let (seek_tick, _) = report.seeks[0];
    report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .copied()
        .collect()
}

/// Звука записано примерно `expected` (± `tolerance`), и он не тишина.
pub fn assert_audio_about(report: &PlaybackReport, expected: Duration, tolerance: Duration) {
    let audio = report.audio_duration();
    assert!(
        audio.abs_diff(expected) <= tolerance,
        "audio {audio:?}, expected about {expected:?}"
    );
    assert!(report.audio_rms() > 0.05, "the audio is silent");
}
