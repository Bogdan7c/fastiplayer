//! Звук эфира ждёт своего времени (мост 16d1, решение 322, общий бэклог п. 51).
//!
//! Находка 16d1: на перезапуске эфира страницей (Twitch, решение 194) первый звук отрезка
//! оказался на секунды дальше базы часов (полный сброс выбросил звук, прочитанный впрок).
//! Правило «звук ждёт своего PTS» (`audio_gap/scheduled_audio_start.rs`) раньше работало
//! только у записи: эфир писал такой звук прямо в базу часов — звук уходил вперёд
//! картинки на весь разрыв. Тесты закрепляют:
//! - эфир с настоящим табло шкалы, перемотка посреди игры (output уже есть): звук цели не
//!   пишется в базу, время ведёт видео, звук входит в своём PTS — шкала остаётся эфиром;
//! - пока видео ждёт смены backend-а, правила звука не считают media «audio-only»: часы не
//!   переносятся на первый звук (корень сдвига позиции в окне);
//! - граница решения: дыры ПОСРЕДИ эфира правило не трогает (поведение прежнее).

use std::num::NonZeroU64;
use std::time::Instant;

use media_core::{
    DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelinePortGeneration,
    DynamicMediaTimelinePublisher, DynamicMediaTimelineState, TimelineMode, TimelineRange,
    dynamic_media_timeline,
};

use super::scheduled_audio_start::{
    AUDIO_TRACK, LATE_AUDIO, PACKET_FRAMES, SAMPLE_RATE, VIDEO_TRACK, enqueue_audio_packet,
    feed_audio_packet, tone_decoder, written_frames, written_pcm,
};
use super::test_support::*;
use super::*;

use crate::pipeline::PresentationClockSource;
use crate::session::audio_gap::scheduled_audio_start::SegmentStartAudioSchedule;
use crate::session::audio_gap::{DemuxAudioPriorityWait, PendingAudioOutputAdmission};
use crate::session::audio_runtime::leading_silence::AudioSegmentStartPlan;
use crate::{PlaybackRate, PreparedMedia};

/// Эпоха источника эфира: тест публикует в одной эпохе.
const EPOCH: DynamicMediaTimelineEpoch = DynamicMediaTimelineEpoch::new(1);

/// Начало окна перемотки эфира (DVR).
const LIVE_WINDOW_START: Duration = Duration::from_secs(20);

/// Край эфира: при установке часы встают сюда (точка входа = край).
const LIVE_EDGE: Duration = Duration::from_secs(60);

/// Окно эфира 20–60 с с перемоткой.
fn live_window() -> DynamicMediaTimelineState {
    let range = TimelineRange::new(
        MediaTime::from_duration(LIVE_WINDOW_START),
        MediaTime::from_duration(LIVE_EDGE),
    )
    .expect("ordered test range");
    DynamicMediaTimelineState::with_dvr(MediaTime::from_duration(LIVE_EDGE), range)
        .expect("valid test DVR state")
}

/// Сессия с установленным эфиром (настоящий порт табло), выбранными видео и звуком.
///
/// Публикатор возвращается, чтобы порт табло оставался подключённым весь тест.
fn live_session_with_audio() -> (
    PlayerSession,
    ScriptedAudioOutputFactoryHandle,
    DynamicMediaTimelinePublisher,
) {
    let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
        port_generation: DynamicMediaTimelinePortGeneration::new(NonZeroU64::MIN),
        source_epoch: EPOCH,
        state: live_window().into(),
    });
    let demuxer = FakeDemuxer::new(
        vec![fake_track(1, TrackKind::Video)],
        None,
        Arc::new(Mutex::new(Vec::new())),
    );
    let prepared_media = PreparedMedia::from_external_label("fake-live", Box::new(demuxer))
        .with_dynamic_timeline(port)
        .expect("media without a playback window accepts a timeline port");
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(output_factory);
    session.load_prepared_media_with_autoplay(prepared_media, false);
    session
        .pipeline
        .select_video_track(VIDEO_TRACK, VideoDecodeRequirement::new(VideoCodec::H264));
    session.pipeline.select_audio_track(AUDIO_TRACK);
    session.pipeline.install_audio_decoder(tone_decoder());
    assert_eq!(
        session.snapshot().timeline.mode,
        TimelineMode::Live,
        "precondition: the media is installed as a live stream"
    );
    (session, output_factory_handle, publisher)
}

/// Перемотка посреди игры: новое поколение, очищенный output, часы на цели.
fn land_seek_mid_play(session: &mut PlayerSession, seek_target: Duration) {
    let generation = session.pipeline.begin_seek_generation();
    let _clear_ack = session.pipeline.clear_audio_output_for_seek(generation);
    session.pipeline.reset_clocks_for_seek(seek_target);
}

#[test]
fn a_live_seek_with_an_output_lets_late_audio_wait_for_its_pts() {
    let (mut session, output_factory_handle, _publisher) = live_session_with_audio();
    // Звук уже играл с края эфира: output есть. Перемотка в окно на 30 с, звук цели — с 32 с
    // (как новая шкала после перезапуска эфира страницей).
    feed_audio_packet(&mut session, LIVE_EDGE);
    let seek_target = Duration::from_secs(30);
    land_seek_mid_play(&mut session, seek_target);
    let audio_start = seek_target + LATE_AUDIO;
    enqueue_audio_packet(&mut session, audio_start);
    session.set_playback_state(PlaybackState::Playing);
    output_factory_handle
        .last_output_handle()
        .expect("output")
        .set_buffer_level_ms(0.0);
    let pcm_before = written_frames(&written_pcm(&output_factory_handle));

    session.process_pending_audio_packets();
    session.hand_over_presentation_clock_for_audio_gap_if_proven(
        Instant::now(),
        DemuxAudioPriorityWait::NotSaturated,
    );

    assert_eq!(
        written_frames(&written_pcm(&output_factory_handle)),
        pcm_before,
        "the audio of the target region is not written at the seek target"
    );
    assert_eq!(
        session.pipeline.presentation_clock_source(),
        PresentationClockSource::MonotonicAfterAudioEnd,
        "video drives the time until the audio is due: an empty output would freeze it"
    );

    // Показ дошёл до звука цели: звук забирает часы с текущей позиции.
    let now = Instant::now();
    session.pipeline.start_monotonic_media_clock(
        seek_target,
        now.checked_sub(LATE_AUDIO).expect("test instant"),
        PlaybackRate::NORMAL,
    );
    session.resume_audio_after_gap_if_due(now);
    session.process_pending_audio_packets();

    assert_eq!(session.pipeline.media_clock_base(), audio_start);
    assert_eq!(
        session.pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput
    );
    assert_eq!(
        written_frames(&written_pcm(&output_factory_handle)) - pcm_before,
        PACKET_FRAMES,
        "the audio of the target region is written once it is due"
    );
    assert_eq!(
        session.snapshot().timeline.mode,
        TimelineMode::Live,
        "the rule does not touch the timeline"
    );
}

#[test]
fn audio_waiting_for_a_video_backend_switch_does_not_move_the_clock_to_itself() {
    let (mut session, output_factory_handle, _publisher) = live_session_with_audio();
    // Полная смена списка дорожек сняла выбор, звук выбран заново, а видео ждёт backend,
    // который его потянет (так в окне браузерного режима после смены качества).
    session.pipeline.clear_selected_tracks();
    session.pipeline.select_audio_track(AUDIO_TRACK);
    session.pipeline.install_audio_decoder(tone_decoder());
    session.request_video_backend_reselection(
        VideoDecodeRequirement::new(VideoCodec::H264),
        VIDEO_TRACK,
    );
    let clock_base = session.pipeline.media_clock_base();
    let audio_start = clock_base + LATE_AUDIO;
    enqueue_audio_packet(&mut session, audio_start);

    assert!(session.video_continues_the_presentation());
    assert_eq!(
        session.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::ScheduledLater {
            clock_base,
            audio_start,
        }
    );
    assert_ne!(
        session.plan_audio_segment_start(audio_start, SAMPLE_RATE),
        AudioSegmentStartPlan::StartClockAtFirstSample {
            first_sample_time: audio_start,
            gap: LATE_AUDIO,
        },
        "an audio-only plan would move the clock to the first audio sample"
    );

    session.set_playback_state(PlaybackState::Playing);
    session.process_pending_audio_packets();

    assert_eq!(
        session.pipeline.media_clock_base(),
        clock_base,
        "the clock stays where the picture is"
    );
    assert_eq!(
        output_factory_handle.create_count(),
        0,
        "the late audio waits compressed instead of creating an output"
    );
}

#[test]
fn a_gap_in_the_middle_of_a_live_stream_is_not_held() {
    let (mut session, output_factory_handle, _publisher) = live_session_with_audio();
    session.set_playback_state(PlaybackState::Playing);
    // Отрезок уже звучит с края эфира; следующий packet — на 2 с дальше (дыра посреди).
    feed_audio_packet(&mut session, LIVE_EDGE);
    enqueue_audio_packet(&mut session, LIVE_EDGE + LATE_AUDIO);
    output_factory_handle
        .last_output_handle()
        .expect("output")
        .set_buffer_level_ms(0.0);

    assert_eq!(
        session.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::NotApplicable,
        "the rule covers only the start of a segment"
    );
    assert_eq!(
        session.pending_audio_output_admission(),
        PendingAudioOutputAdmission::Admit,
        "gaps in the middle of a live stream keep the old behaviour (decision 322 scope)"
    );
}
