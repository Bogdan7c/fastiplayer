//! «Зеркало браузера» до кадра: предохранитель памяти полки выкидывает видео, а
//! звук держит, и перемотка в выкинутое место доезжает, когда страница
//! перекачивает ТОЛЬКО видео.
//!
//! Живая находка 04b (YouTube 4K60): старый предохранитель выкидывал самое старое
//! сразу в обеих дорожках. Браузер же у себя выкидывает тяжёлое видео, а лёгкий
//! звук держит минутами (замер 1.11.2026: Chromium — до 150 МиБ видео и 12 МиБ
//! звука на дорожку) и при возврате в пройденное место звук второй раз не
//! присылает. Звука у цели не было нигде — перемотка висела до `SeekTimeout`.
//! Здесь то же в масштабе фикстур: потолок памяти потока подобран так, что во
//! время игры предохранитель выселяет сыгранное видео; перемотка назад туда
//! уходит к странице, а страница (как браузер) присылает только видео.
//!
//! Настоящие декодеры видео и звука, настоящее время (обвязка `support/playback`).
//! Фикстуры: `video-h264-64-4s.mp4` (4 с, 25 к/с, десять фрагментов по 0,4 с,
//! каждый с ключевого кадра, ~8 КБ) и `audio-aac-4s.mp4` (~1,9 КБ на 0,4 с).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::num::NonZeroUsize;
use std::time::Duration;

use player_core::{PlaybackState, PreparedMedia};
use pushed_media_source::{
    BufferIntake, LaneId, PageSeekRequest, PushedContainer, PushedDuration, PushedSourcePolicy,
    ShelfSeekOutlook, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::play_controlled;

/// Длительность кадра фикстур (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Длительность роликов.
const CLIP: Duration = Duration::from_secs(4);
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);
/// Жёсткий потолок памяти потока: весь звук (~19 КБ) и ~1,6 с видео вперёд
/// помещаются, порог «полон» (75 %, 48 КБ) — нет: сыгранное видео выселяется.
const STREAM_CEILING_BYTES: usize = 64 * 1024;
/// Сколько видео вперёд держит страница (как плеер страницы: докачивает, пока
/// запас меньше своей цели и источник не «полон»).
const PAGE_AHEAD_TARGET: Duration = Duration::from_millis(1_200);
/// Длительность фрагмента фикстур.
const FRAGMENT: Duration = Duration::from_millis(400);

/// Страница с честным плеером: весь звук сразу (он лёгкий, браузер держит его
/// весь), видео — по фрагменту, пока запас меньше цели и источник не «полон».
/// После перемотки видео качается заново с фрагмента цели, звук — нет: он лежит
/// в буфере браузера, а у нас — на полке.
struct HonestPage {
    writer: pushed_media_source::PushedStreamWriter,
    video: FragmentedMp4,
    next_video_fragment: usize,
}

impl HonestPage {
    fn feed_video(&mut self, monitor: &pushed_media_source::PushedStreamMonitor) {
        while self.next_video_fragment < self.video.fragments.len() {
            let status = monitor.buffer_status();
            if status.intake == BufferIntake::Full || status.buffered_ahead >= PAGE_AHEAD_TARGET {
                return;
            }
            let fragment = &self.video.fragments[self.next_video_fragment];
            self.writer.append(VIDEO, fragment).expect("video");
            self.next_video_fragment += 1;
        }
        if self.next_video_fragment == self.video.fragments.len() {
            // Всё видео отдано: конец потока, как `endOfStream` страницы.
            self.writer.end_of_stream().expect("end of stream");
            self.next_video_fragment += 1;
        }
    }

    fn seek_to(&mut self, target: Duration) {
        let fragment = target.as_millis() / FRAGMENT.as_millis();
        self.next_video_fragment = usize::try_from(fragment).expect("fragment index");
    }
}

#[test]
fn a_seek_back_to_evicted_video_lands_with_sound_when_the_page_resends_only_video() {
    let policy = PushedSourcePolicy::recommended()
        .expect("policy")
        .with_max_held_stream_bytes(NonZeroUsize::new(STREAM_CEILING_BYTES).expect("non-zero"));
    let full_at = policy.buffer_full_at_held_bytes();
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer.set_duration(PushedDuration::Finite(CLIP));
    let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
    writer
        .append(VIDEO, &video.initialization)
        .expect("video init");
    writer
        .append(AUDIO, &audio.initialization)
        .expect("audio init");
    for audio_fragment in &audio.fragments {
        writer.append(AUDIO, audio_fragment).expect("audio");
    }
    let monitor = writer.monitor();
    let mut page = HonestPage {
        writer,
        video,
        next_video_fragment: 0,
    };
    page.feed_video(&monitor);

    let seek_from = Duration::from_millis(3_000);
    let target = Duration::from_millis(200);
    let mut requests: Vec<PageSeekRequest> = Vec::new();
    let mut outlook_before_seek = None;
    let mut seeked = false;
    let prepared = PreparedMedia::from_external_label(
        "pushed media mirror guard",
        Box::new(support::playback::open(&mut opener)),
    );
    let report = play_controlled(prepared, |control| {
        // Как приложение: позиция плеера → монитор источника каждый шаг.
        let position = control.observation().position;
        monitor.note_playback_position(position);
        if let Some(request) = monitor.take_page_seek_request() {
            page.seek_to(request.target);
            requests.push(request);
        }
        if !seeked && position >= seek_from {
            seeked = true;
            outlook_before_seek = Some(monitor.seek_outlook(target));
            control.seek(target);
        }
        page.feed_video(&monitor);
    });

    assert!(seeked, "playback reached {seek_from:?}");
    assert!(
        monitor.held_segment_bytes() < STREAM_CEILING_BYTES,
        "never above the hard ceiling"
    );
    assert_eq!(
        outlook_before_seek,
        Some(ShelfSeekOutlook::AsksPage),
        "the memory guard evicted the played video at the target (threshold {full_at} bytes)"
    );
    assert_eq!(
        requests,
        [PageSeekRequest { target }],
        "the evicted video is asked from the page exactly once"
    );
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);

    // Картинка: первый кадр после перемотки — у цели, дальше по порядку до конца.
    let (seek_tick, _) = report.seeks[0];
    let after: Vec<_> = report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .collect();
    let first = after.first().expect("frames after the seek");
    assert!(
        first.pts.abs_diff(target) <= FRAME,
        "first frame after the seek at {:?}, target {target:?}",
        first.pts
    );
    assert_eq!(after.last().map(|frame| frame.pts), Some(CLIP - FRAME));

    // Звук: после очистки выхода перемоткой звучит всё время от цели до конца —
    // звук у цели остался на полке, страница его не присылала.
    let (sample_rate, channels) = report.audio_format.expect("audio played");
    let first_after_seek = *report
        .audio_frames_at_seek_clears
        .last()
        .expect("the seek cleared the audio output");
    let frames_after_seek = report.audio_samples.len() / channels - first_after_seek;
    let audio_after_seek =
        Duration::from_secs_f64(frames_after_seek as f64 / f64::from(sample_rate));
    let expected = CLIP - target;
    assert!(
        audio_after_seek + FRAME * 3 >= expected,
        "sound after the seek lasted {audio_after_seek:?}, expected about {expected:?}"
    );
    assert!(report.audio_rms() > 0.05, "the sound is not silence");
}
