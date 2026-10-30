//! Первый звук отрезка дальше базы часов больше потолка тишины: звук ждёт своего PTS.
//!
//! Проверяется наблюдаемое: создан ли output, что в него записано (тишина/звук), куда
//! стала база часов, кто ведёт время показа, в каком состоянии playback. Сквозной
//! сценарий «до кадра» с настоящими декодерами — `pushed-media-source/tests/
//! audio_large_gap_to_frame.rs`.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;

use super::audio_packet_window::RecordingPcmDecoder;
use super::audio_runtime::FakeTempoFactory;
use super::test_support::*;
use super::*;
use media_core::TimelineMode;

use crate::pipeline::PresentationClockSource;
use crate::seek_state::PlaybackResumeIntent;
use crate::session::audio_gap::scheduled_audio_start::{
    EarliestContentStart, PrerollAudioGate, ScheduledAudioJoinOutcome, SegmentStartAudioSchedule,
};
use crate::session::audio_gap::{DemuxAudioPriorityWait, PendingAudioOutputAdmission};
use crate::session::audio_runtime::SeekAudioGateStatus;
use crate::session::audio_runtime::leading_silence::AudioSegmentStartPlan;
use crate::{PendingAudioPacket, PlaybackRate};

/// Частота PCM тестового звука.
const SAMPLE_RATE: u32 = 48_000;

/// Каналов в тестовом PCM.
const CHANNELS: u32 = 2;

/// PCM frames одного decoded packet-а (20 мс при 48 кГц).
const PACKET_FRAMES: usize = 960;

/// Значение каждого sample-а decoded packet-а: заведомо не тишина.
const TONE: f32 = 0.5;

/// Выбранная аудиодорожка.
const AUDIO_TRACK: TrackId = TrackId::new(2);

/// Выбранная видеодорожка.
const VIDEO_TRACK: TrackId = TrackId::new(1);

/// Звук начинается на 2 с позже базы (больше потолка тишины 1 с).
const LATE_AUDIO: Duration = Duration::from_secs(2);

/// Цель preroll-а звука для ворот старта (как `PlayerTickConfig` по умолчанию).
const AUDIO_PREROLL_TARGET_MS: f64 = 50.0;

/// PCM frames в длительности (тестовая частота).
fn frames_in(duration: Duration) -> usize {
    usize::try_from(duration.as_nanos() * u128::from(SAMPLE_RATE) / 1_000_000_000)
        .expect("test durations fit usize")
}

/// Decoder, который на каждый packet отдаёт 20 мс «тона».
fn tone_decoder() -> Box<RecordingPcmDecoder> {
    Box::new(RecordingPcmDecoder::new(
        Arc::new(Mutex::new(Vec::new())),
        vec![TONE; PACKET_FRAMES * CHANNELS as usize],
        SAMPLE_RATE,
        CHANNELS,
    ))
}

/// Есть ли в media выбранное видео.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedVideo {
    /// A/V media.
    Selected,
    /// Audio-only media.
    None,
}

/// Session с выбранным звуком (decoder готов, output ленивый) и, по желанию, видео.
fn session_with_audio(
    selected_video: SelectedVideo,
) -> (PlayerSession, ScriptedAudioOutputFactoryHandle) {
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(output_factory);
    if selected_video == SelectedVideo::Selected {
        session
            .pipeline
            .select_video_track(VIDEO_TRACK, VideoDecodeRequirement::new(VideoCodec::H264));
    }
    session.pipeline.select_audio_track(AUDIO_TRACK);
    session.pipeline.install_audio_decoder(tone_decoder());
    (session, output_factory_handle)
}

/// Кладёт audio packet текущего поколения в очередь (как demux).
fn enqueue_audio_packet(session: &mut PlayerSession, packet_pts: Duration) {
    let generation = session.pipeline.seek_generation();
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
            AUDIO_TRACK,
            packet_pts,
            None,
            None,
            generation,
            Bytes::from_static(b"aac-frame"),
        ));
}

/// Отдаёт один audio packet прямо в decode/output (минуя очередь).
fn feed_audio_packet(session: &mut PlayerSession, packet_pts: Duration) {
    let generation = session.pipeline.seek_generation();
    session.process_audio_packet(
        AUDIO_TRACK,
        packet_pts,
        None,
        None,
        generation,
        b"aac-frame",
    );
}

/// PCM, записанный в последний созданный output.
fn written_pcm(output_factory_handle: &ScriptedAudioOutputFactoryHandle) -> Vec<f32> {
    output_factory_handle
        .last_output_handle()
        .expect("audio output must be created by the first PCM")
        .written_samples()
}

/// Сколько PCM frames тишины записано перед первым слышимым sample-ом.
fn leading_silence_frames(pcm: &[f32]) -> usize {
    let (stereo_frames, _) = pcm.as_chunks::<{ CHANNELS as usize }>();
    stereo_frames
        .iter()
        .take_while(|frame| frame.iter().all(|sample| *sample == 0.0))
        .count()
}

/// Сколько PCM frames записано всего.
fn written_frames(pcm: &[f32]) -> usize {
    pcm.len() / CHANNELS as usize
}

/// Запускает monotonic clock так, будто показ идёт с базы уже `elapsed`.
fn run_presentation_for(session: &mut PlayerSession, elapsed: Duration, now: Instant) {
    session.set_playback_state(PlaybackState::Playing);
    let base = session.pipeline.media_clock_base();
    session.pipeline.start_monotonic_media_clock(
        base,
        now.checked_sub(elapsed).expect("test instant"),
        PlaybackRate::NORMAL,
    );
}

#[test]
fn late_audio_beyond_the_silence_cap_is_held_without_creating_an_output() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    enqueue_audio_packet(&mut session, LATE_AUDIO);

    session.process_pending_audio_packets();

    assert_eq!(
        session.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::ScheduledLater {
            clock_base: Duration::ZERO,
            audio_start: LATE_AUDIO,
        }
    );
    assert_eq!(
        session.pending_audio_output_admission(),
        PendingAudioOutputAdmission::HoldUntilScheduledAudioStart
    );
    assert_eq!(
        output_factory_handle.create_count(),
        0,
        "the held packet must not create an output that would play it at the clock base"
    );
    assert_eq!(
        session.pipeline.front_pending_audio_packet_start(),
        Some(LATE_AUDIO),
        "the packet waits in the queue"
    );
    assert_eq!(session.pipeline.media_clock_base(), Duration::ZERO);
}

#[test]
fn audio_within_the_silence_cap_is_not_held_and_gets_leading_silence() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    let audio_start = Duration::from_millis(600);
    enqueue_audio_packet(&mut session, audio_start);

    assert_eq!(
        session.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::WithinSilenceCap
    );
    session.process_pending_audio_packets();

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(leading_silence_frames(&pcm), frames_in(audio_start));
    assert_eq!(session.pipeline.front_pending_audio_packet_start(), None);
}

#[test]
fn hold_applies_only_to_the_first_audio_of_an_av_segment_on_a_static_timeline() {
    // Audio-only: смотреть нечего — звук не ждёт, часы переносятся к нему.
    let (mut audio_only, _audio_only_outputs) = session_with_audio(SelectedVideo::None);
    enqueue_audio_packet(&mut audio_only, LATE_AUDIO);
    assert_eq!(
        audio_only.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::NotApplicable
    );

    // Отрезок уже начат: дальний packet — разрыв посреди звука (`audio_gap.rs`).
    let (mut started, _started_outputs) = session_with_audio(SelectedVideo::Selected);
    feed_audio_packet(&mut started, Duration::ZERO);
    enqueue_audio_packet(&mut started, LATE_AUDIO);
    assert_eq!(
        started.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::NotApplicable
    );

    // Live-timeline: база — край эфира, поведение прежнее.
    let (mut live, _live_outputs) = session_with_audio(SelectedVideo::Selected);
    live.snapshot.timeline.mode = TimelineMode::Live;
    enqueue_audio_packet(&mut live, LATE_AUDIO);
    assert_eq!(
        live.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::NotApplicable
    );
    assert_eq!(
        live.pending_audio_output_admission(),
        PendingAudioOutputAdmission::Admit
    );
}

#[test]
fn implausible_gap_keeps_the_old_write_at_the_clock_base() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    session.set_snapshot_duration(Some(Duration::from_secs(4)));
    // Метка «из другой эпохи»: за концом media.
    let broken_audio_start = Duration::from_secs(3 * 60 * 60);
    enqueue_audio_packet(&mut session, broken_audio_start);

    assert!(matches!(
        session.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::ImplausibleGap { .. }
    ));
    assert!(matches!(
        session.plan_audio_segment_start(broken_audio_start, SAMPLE_RATE),
        AudioSegmentStartPlan::ImplausibleGap { .. }
    ));
    session.process_pending_audio_packets();

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(leading_silence_frames(&pcm), 0, "no silence, no wait");
    assert_eq!(written_frames(&pcm), PACKET_FRAMES);
    assert_eq!(session.pipeline.media_clock_base(), Duration::ZERO);
    assert_eq!(session.snapshot().last_error, None);
}

#[test]
fn start_gate_starts_video_first_while_audio_is_scheduled_later() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    session.set_playback_state(PlaybackState::Buffering);
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::ZERO, 1));
    enqueue_audio_packet(&mut session, LATE_AUDIO);

    assert_eq!(
        session.preroll_audio_gate(AUDIO_PREROLL_TARGET_MS),
        PrerollAudioGate::ScheduledLater {
            audio_start: LATE_AUDIO
        }
    );
    let started = session
        .finish_autoplay_preroll_if_ready(AUDIO_PREROLL_TARGET_MS)
        .expect("preroll finish");

    assert!(started, "the start must not wait for audio due in 2 s");
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(output_factory_handle.create_count(), 0);
    assert_eq!(
        session.pipeline.presentation_clock_source(),
        PresentationClockSource::MonotonicWithoutAudio
    );
    assert!(
        session
            .pipeline
            .monotonic_media_position(Instant::now())
            .is_some(),
        "video runs on the monotonic clock from the base"
    );
}

#[test]
fn start_gate_still_waits_for_audio_within_the_silence_cap() {
    let (mut session, _output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    session.set_playback_state(PlaybackState::Buffering);
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::ZERO, 1));
    enqueue_audio_packet(&mut session, Duration::from_millis(600));

    assert!(matches!(
        session.preroll_audio_gate(AUDIO_PREROLL_TARGET_MS),
        PrerollAudioGate::Readiness(_)
    ));
    let started = session
        .finish_autoplay_preroll_if_ready(AUDIO_PREROLL_TARGET_MS)
        .expect("preroll finish");

    assert!(!started);
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
}

#[test]
fn scheduled_audio_joins_when_the_presentation_reaches_its_pts() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    enqueue_audio_packet(&mut session, LATE_AUDIO);
    let now = Instant::now();

    run_presentation_for(&mut session, Duration::from_secs(1), now);
    let early = session.join_scheduled_audio_start_if_due(now);
    session.process_pending_audio_packets();

    assert_eq!(
        early,
        ScheduledAudioJoinOutcome::NotDue {
            presentation_position: Duration::from_secs(1),
            audio_start: LATE_AUDIO,
        }
    );
    assert_eq!(output_factory_handle.create_count(), 0, "still held at 1 s");
    assert_eq!(session.pipeline.media_clock_base(), Duration::ZERO);

    // Tick шагает по 5 мс: время показа перешло PTS звука на 5 мс.
    let overshoot = Duration::from_millis(5);
    run_presentation_for(&mut session, LATE_AUDIO + overshoot, now);
    let joined = session.join_scheduled_audio_start_if_due(now);
    session.process_pending_audio_packets();

    assert_eq!(
        joined,
        ScheduledAudioJoinOutcome::Joined {
            joined_at: LATE_AUDIO + overshoot,
            audio_start: LATE_AUDIO,
        }
    );
    assert_eq!(session.pipeline.media_clock_base(), LATE_AUDIO + overshoot);
    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        leading_silence_frames(&pcm),
        0,
        "joined audio is never padded"
    );
    assert_eq!(
        written_frames(&pcm),
        PACKET_FRAMES - frames_in(overshoot),
        "PCM before the join position is trimmed: the first sample plays at its own time"
    );
    let output = output_factory_handle
        .last_output_handle()
        .expect("output created");
    assert_eq!(output.play_count(), 1, "the output joins the running show");
    assert_eq!(
        session.pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput,
        "the audio clock takes over after the join"
    );
}

#[test]
fn scheduled_audio_never_joins_while_the_presentation_is_not_running() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    enqueue_audio_packet(&mut session, LATE_AUDIO);
    let now = Instant::now();
    run_presentation_for(&mut session, LATE_AUDIO * 2, now);
    session.set_playback_state(PlaybackState::Paused);

    let outcome = session.join_scheduled_audio_start_if_due(now);

    assert_eq!(outcome, ScheduledAudioJoinOutcome::PresentationNotRunning);
    assert_eq!(session.pipeline.media_clock_base(), Duration::ZERO);
    assert_eq!(output_factory_handle.create_count(), 0);
}

#[test]
fn seek_landing_with_an_output_hands_the_clock_over_and_resumes_at_the_audio_pts() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    // Звук уже играл с нуля: output есть. Seek на 1 с, звук цели — с 3 с.
    feed_audio_packet(&mut session, Duration::ZERO);
    let seek_target = Duration::from_secs(1);
    let generation = session.pipeline.begin_seek_generation();
    let _clear_ack = session.pipeline.clear_audio_output_for_seek(generation);
    session.pipeline.reset_clocks_for_seek(seek_target);
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
        session.pipeline.presentation_clock_source(),
        PresentationClockSource::MonotonicAfterAudioEnd,
        "an empty output would freeze the clock at the target"
    );
    assert_eq!(
        written_frames(&written_pcm(&output_factory_handle)),
        pcm_before,
        "audio of the target region is not written at the target"
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
    let pcm_after = written_pcm(&output_factory_handle);
    assert_eq!(
        written_frames(&pcm_after) - pcm_before,
        PACKET_FRAMES,
        "the audio of the target region is written once it is due"
    );
}

#[test]
fn seek_audio_gate_is_released_only_after_the_buffer_clear() {
    let (mut held, _held_outputs) = session_with_audio(SelectedVideo::Selected);
    enqueue_audio_packet(&mut held, LATE_AUDIO);
    let (mut on_time, _on_time_outputs) = session_with_audio(SelectedVideo::Selected);
    enqueue_audio_packet(&mut on_time, Duration::ZERO);

    for waiting in [
        SeekAudioGateStatus::WaitingForDecoder,
        SeekAudioGateStatus::WaitingForOutput,
        SeekAudioGateStatus::WaitingForPreroll,
    ] {
        assert!(
            held.scheduled_audio_start_releases_seek_audio_gate(waiting),
            "{waiting:?}"
        );
        assert!(
            !on_time.scheduled_audio_start_releases_seek_audio_gate(waiting),
            "{waiting:?}: audio due now must still fill the buffer"
        );
    }
    assert!(
        !held.scheduled_audio_start_releases_seek_audio_gate(SeekAudioGateStatus::WaitingForClear),
        "pre-seek audio may still sit in the ring"
    );
}

#[test]
fn seek_commit_needs_no_output_only_for_scheduled_audio() {
    let (mut held, _held_outputs) = session_with_audio(SelectedVideo::Selected);
    enqueue_audio_packet(&mut held, LATE_AUDIO);
    let (mut with_output, _with_output_outputs) = session_with_audio(SelectedVideo::Selected);
    feed_audio_packet(&mut with_output, Duration::ZERO);
    let generation = with_output.pipeline.begin_seek_generation();
    let _clear_ack = with_output.pipeline.clear_audio_output_for_seek(generation);
    enqueue_audio_packet(&mut with_output, LATE_AUDIO);

    assert!(held.seek_commit_defers_audio_output_to_scheduled_start());
    assert!(
        !with_output.seek_commit_defers_audio_output_to_scheduled_start(),
        "an existing output is resumed as usual"
    );
}

#[test]
fn lazily_created_output_plays_only_while_the_presentation_runs() {
    let (mut session, _output_factory_handle) = session_with_audio(SelectedVideo::Selected);

    for (state, joins) in [
        (PlaybackState::Playing, true),
        (PlaybackState::Buffering, false),
        (PlaybackState::Paused, false),
        (PlaybackState::Seeking, false),
    ] {
        session.set_playback_state(state);
        assert_eq!(
            session.lazily_created_audio_output_joins_running_presentation(),
            joins,
            "{state:?}"
        );
    }

    // EOF drain первого старта, который ещё не выполнен: output запустит сам старт.
    session.set_playback_state(PlaybackState::Buffering);
    session.mark_startup_autoplay_preroll_pending();
    session.enter_eof_drain();
    assert!(!session.lazily_created_audio_output_joins_running_presentation());
}

#[test]
fn scheduled_audio_joining_after_source_eof_starts_its_output() {
    // Короткий ролик: источник кончился раньше, чем показ дошёл до звука. Показ идёт в
    // `Draining`, и созданный там output обязан заиграть сам — иначе часы звука стоят,
    // а очередь звука не убывает (вечный `Draining`).
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    enqueue_audio_packet(&mut session, LATE_AUDIO);
    let now = Instant::now();
    run_presentation_for(&mut session, LATE_AUDIO, now);
    session.enter_eof_drain();
    assert_eq!(session.playback_state(), PlaybackState::Draining);

    let joined = session.join_scheduled_audio_start_if_due(now);
    session.process_pending_audio_packets();

    assert!(matches!(joined, ScheduledAudioJoinOutcome::Joined { .. }));
    let output = output_factory_handle
        .last_output_handle()
        .expect("output created by the joined audio");
    assert_eq!(output.play_count(), 1, "the output plays inside EOF drain");
}

#[test]
fn audio_only_far_start_moves_the_clock_to_its_first_sample() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::None);
    session.set_snapshot_duration(Some(Duration::from_secs(4)));

    feed_audio_packet(&mut session, LATE_AUDIO);

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(leading_silence_frames(&pcm), 0, "audio-only: no silence");
    assert_eq!(session.pipeline.media_clock_base(), LATE_AUDIO);
    assert_eq!(
        session.snapshot().current_position,
        LATE_AUDIO,
        "the position shows the time of the sound being heard"
    );
}

#[test]
fn audio_only_start_beyond_the_media_end_keeps_the_clock_base() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::None);
    session.set_snapshot_duration(Some(Duration::from_secs(4)));
    let broken_audio_start = Duration::from_secs(5);

    assert!(matches!(
        session.plan_audio_segment_start(broken_audio_start, SAMPLE_RATE),
        AudioSegmentStartPlan::ImplausibleGap { .. }
    ));
    feed_audio_packet(&mut session, broken_audio_start);

    assert_eq!(session.pipeline.media_clock_base(), Duration::ZERO);
    assert_eq!(session.snapshot().current_position, Duration::ZERO);
    assert_eq!(
        written_frames(&written_pcm(&output_factory_handle)),
        PACKET_FRAMES
    );
}

/// Audio-only seek: commit после записи первого PCM цели.
fn commit_audio_only_seek(first_audio_after_target: Duration) -> PlayerSession {
    let (mut session, _output_factory_handle) = session_with_audio(SelectedVideo::None);
    let seek_commit = audio_gate_seek_commit(PlaybackResumeIntent::Play);
    let target = seek_commit.target_position.as_duration();
    session.set_snapshot_duration(Some(Duration::from_secs(30)));
    session.set_playback_state(PlaybackState::Seeking);
    session.seek_runtime.set_active_commit(seek_commit);
    session.pipeline.set_media_clock_base(target);

    feed_audio_packet(&mut session, target + first_audio_after_target);
    assert_eq!(
        session.pipeline.media_clock_base(),
        target,
        "during the seek the base stays at the target: the commit owns it"
    );
    session.complete_seek_commit(seek_commit);
    session
}

#[test]
fn audio_only_seek_commits_at_the_first_audio_sample() {
    let gap = Duration::from_millis(1_500);
    let target = audio_gate_seek_commit(PlaybackResumeIntent::Play)
        .target_position
        .as_duration();

    let session = commit_audio_only_seek(gap);

    assert_eq!(session.snapshot().last_error, None);
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(session.pipeline.media_clock_base(), target + gap);
    assert_eq!(
        session.snapshot().current_position,
        target + gap,
        "the position is the time of the first sample heard after the seek"
    );
}

#[test]
fn audio_only_seek_landing_on_time_commits_at_the_target() {
    let target = audio_gate_seek_commit(PlaybackResumeIntent::Play)
        .target_position
        .as_duration();

    let session = commit_audio_only_seek(Duration::from_millis(10));

    assert_eq!(session.pipeline.media_clock_base(), target);
    assert_eq!(session.snapshot().current_position, target);
}

/// Session в стартовом preroll-е: кадр `first_frame` готов, звук ждёт в `audio_start`.
fn startup_preroll(first_frame: Duration, audio_start: Duration) -> PlayerSession {
    let (mut session, _output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    session.set_playback_state(PlaybackState::Buffering);
    session.mark_startup_autoplay_preroll_pending();
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(first_frame, 1));
    enqueue_audio_packet(&mut session, audio_start);
    session
}

#[test]
fn late_recording_starts_at_its_content_and_audio_starts_normally() {
    // MPEG-TS от ffmpeg: оба потока с ~1,4 с при шкале с нуля.
    let content_start = Duration::from_millis(1_400);
    let mut session = startup_preroll(content_start, content_start + Duration::from_millis(20));

    let started = session
        .finish_autoplay_preroll_if_ready(AUDIO_PREROLL_TARGET_MS)
        .expect("preroll finish");

    assert_eq!(session.pipeline.media_clock_base(), content_start);
    assert_eq!(session.snapshot().current_position, content_start);
    assert_eq!(
        session.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::WithinSilenceCap,
        "after the move the audio is on time: it is not held"
    );
    assert!(
        !started,
        "the start now waits for the audio buffer like any A/V start (audio clock)"
    );
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
}

#[test]
fn late_recording_with_much_later_audio_starts_at_the_first_frame_and_holds_audio() {
    let first_frame = Duration::from_millis(1_400);
    let audio_start = Duration::from_secs(4);
    let mut session = startup_preroll(first_frame, audio_start);

    let rebased = session.start_at_earliest_content_if_late();
    let started = session
        .finish_autoplay_preroll_if_ready(AUDIO_PREROLL_TARGET_MS)
        .expect("preroll finish");

    assert_eq!(
        rebased,
        EarliestContentStart::Rebased {
            previous_base: Duration::ZERO,
            content_start: first_frame,
        }
    );
    assert!(started, "video first from its first frame");
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(
        session.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::ScheduledLater {
            clock_base: first_frame,
            audio_start,
        }
    );
    assert_eq!(
        session.start_at_earliest_content_if_late(),
        EarliestContentStart::NotApplicable,
        "the move happens once"
    );
}

#[test]
fn video_from_the_base_keeps_the_start_at_the_base() {
    let mut session = startup_preroll(Duration::ZERO, LATE_AUDIO);

    assert_eq!(
        session.start_at_earliest_content_if_late(),
        EarliestContentStart::NotApplicable
    );
    assert_eq!(session.pipeline.media_clock_base(), Duration::ZERO);
}

#[test]
fn content_start_move_is_only_for_the_first_start() {
    let content_start = Duration::from_millis(1_400);
    let mut session = startup_preroll(content_start, content_start);
    // Тот же preroll, но первый старт уже выполнен (buffering-resume после игры).
    session.clear_startup_autoplay_preroll();

    assert_eq!(
        session.start_at_earliest_content_if_late(),
        EarliestContentStart::NotApplicable
    );
    assert_eq!(session.pipeline.media_clock_base(), Duration::ZERO);
}

#[test]
fn seek_commit_without_an_output_succeeds_while_audio_is_scheduled_later() {
    // Seek во время старта «видео первым»: звук ещё не входил, output-а нет. Commit с
    // намерением играть не должен считать это потерей устройства.
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    let seek_commit = audio_gate_seek_commit(PlaybackResumeIntent::Play);
    let target = seek_commit.target_position.as_duration();
    session.set_playback_state(PlaybackState::Seeking);
    session.seek_runtime.set_active_commit(seek_commit);
    session.pipeline.set_media_clock_base(target);
    enqueue_audio_packet(&mut session, target + LATE_AUDIO);

    session.complete_seek_commit(seek_commit);

    assert_eq!(session.snapshot().last_error, None);
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(session.snapshot().current_position, target);
    assert_eq!(
        output_factory_handle.create_count(),
        0,
        "the audio still waits for its PTS"
    );
}

/// A/V session с fake media (две звуковые дорожки), показом без звука на позиции
/// `position` и удержанным звуком в `LATE_AUDIO`.
fn playing_fake_media_with_held_audio(
    position: Duration,
) -> (
    PlayerSession,
    ScriptedAudioOutputFactoryHandle,
    [TrackId; 2],
) {
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let session = PlayerSession::with_audio_output_factory(output_factory);
    let (session, tracks) = prepare_fake_media_with_held_audio(session, position);
    (session, output_factory_handle, tracks)
}

/// Ставит в `session` fake media (видео и две звуковые дорожки), кадр на экране,
/// удержанный звук в `LATE_AUDIO` и показ без звука на позиции `position`.
fn prepare_fake_media_with_held_audio(
    mut session: PlayerSession,
    position: Duration,
) -> (PlayerSession, [TrackId; 2]) {
    let second_audio_track = TrackId::new(3);
    let _seek_log = install_fake_media(
        &mut session,
        vec![
            fake_track(VIDEO_TRACK.get(), TrackKind::Video),
            fake_track(AUDIO_TRACK.get(), TrackKind::Audio),
            fake_track(second_audio_track.get(), TrackKind::Audio),
        ],
    );
    session.pipeline.install_audio_decoder(tone_decoder());
    // Кадр на экране: пауза без него ушла бы искать poster-кадр.
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(position, 1));
    enqueue_audio_packet(&mut session, LATE_AUDIO);
    run_presentation_for(&mut session, position, Instant::now());
    (session, [AUDIO_TRACK, second_audio_track])
}

#[test]
fn delayed_first_pcm_keeps_the_audio_clock_continuous_with_the_presentation() {
    // Вход ровно в PTS звука, но первый PCM появился только через 10 мс показа: база
    // обязана догнать показ, иначе часы звука отбросят позицию назад на 10 мс.
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    enqueue_audio_packet(&mut session, LATE_AUDIO);
    let join_at = LATE_AUDIO;
    let now = Instant::now();
    run_presentation_for(&mut session, join_at, now);
    assert!(matches!(
        session.join_scheduled_audio_start_if_due(now),
        ScheduledAudioJoinOutcome::Joined { .. }
    ));
    let later = now + Duration::from_millis(10);
    let position_before_audio = session.presentation_clock_position_at(later);

    let kept_up = session.join_scheduled_audio_start_if_due(later);
    session.process_pending_audio_packets();

    assert_eq!(
        kept_up,
        ScheduledAudioJoinOutcome::KeptUpWithPresentation {
            previous_base: join_at,
            joined_at: position_before_audio,
        }
    );
    let output = output_factory_handle
        .last_output_handle()
        .expect("output created");
    // Устройство стартует не сразу: пока не прозвучал ни один sample, часы звука стоят
    // на позиции входа — не раньше неё.
    output.set_underlying_output_timing(Duration::ZERO, Duration::from_millis(20));
    let at_device_start = session.presentation_clock_position_at(later);
    output.set_underlying_output_timing(Duration::from_millis(10), Duration::from_millis(20));
    let after_first_samples = session.presentation_clock_position_at(later);

    assert_eq!(
        session.pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput
    );
    assert_eq!(at_device_start, position_before_audio, "no jump back");
    assert_eq!(
        after_first_samples,
        position_before_audio + Duration::from_millis(10),
        "the clock follows the audible audio"
    );
    assert_eq!(
        written_frames(&written_pcm(&output_factory_handle)),
        PACKET_FRAMES - frames_in(position_before_audio - LATE_AUDIO),
        "PCM before the presentation position is trimmed"
    );
}

#[test]
fn pause_while_audio_is_held_freezes_the_wait_and_play_resumes_it() {
    let (mut session, output_factory_handle, _tracks) =
        playing_fake_media_with_held_audio(Duration::from_secs(1));

    session
        .dispatch_command(PlayerCommand::Pause)
        .expect("pause");
    let paused_position = session.snapshot().current_position;
    let while_paused = session.join_scheduled_audio_start_if_due(Instant::now() + LATE_AUDIO);

    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert_eq!(
        while_paused,
        ScheduledAudioJoinOutcome::PresentationNotRunning
    );
    assert!(
        paused_position >= Duration::from_secs(1) && paused_position < LATE_AUDIO,
        "paused at {paused_position:?}"
    );

    session.dispatch_command(PlayerCommand::Play).expect("play");
    let resumed_at = Instant::now();
    let still_held =
        session.join_scheduled_audio_start_if_due(resumed_at + Duration::from_millis(500));
    let due = session.join_scheduled_audio_start_if_due(
        resumed_at + (LATE_AUDIO - paused_position) + Duration::from_millis(5),
    );
    session.process_pending_audio_packets();

    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert!(matches!(
        still_held,
        ScheduledAudioJoinOutcome::NotDue { .. }
    ));
    assert!(matches!(due, ScheduledAudioJoinOutcome::Joined { .. }));
    assert_eq!(
        output_factory_handle.create_count(),
        1,
        "audio joins after the pause"
    );
}

#[test]
fn rate_change_while_audio_is_held_is_applied_and_audio_still_sounds_at_its_pts() {
    // Скорость меняется, пока звук удержан и output-а нет: tempo processor создаст первый
    // PCM. Перепривязка часов при смене скорости переносит базу на позицию показа, и
    // оставшийся до звука разрыв (≤ 1 с) закрывается тишиной — звук всё равно звучит в
    // свой PTS, позиция не прыгает.
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);
    let (mut session, _tracks) =
        prepare_fake_media_with_held_audio(session, Duration::from_secs(1));

    let outcome = session
        .dispatch_command(PlayerCommand::SetPlaybackRate(
            PlaybackRate::new(2.0).expect("valid 2x rate"),
        ))
        .expect("rate command");
    let changed_at = Instant::now();
    let position_at_change = session.presentation_clock_position_at(changed_at);
    let later = changed_at + Duration::from_millis(100);
    let position_later = session.presentation_clock_position_at(later);
    let joined = session.join_scheduled_audio_start_if_due(later);
    session.process_pending_audio_packets();

    assert_eq!(outcome, PlayerCommandOutcome::Applied);
    assert_eq!(session.snapshot().last_error, None);
    assert_eq!(
        position_later - position_at_change,
        Duration::from_millis(200),
        "the video clock runs at the new rate"
    );
    // Смена скорости перепривязала базу к позиции в момент команды (чуть раньше
    // `changed_at`); вход подтягивает её к позиции показа.
    assert!(
        matches!(
            joined,
            ScheduledAudioJoinOutcome::KeptUpWithPresentation { previous_base, joined_at }
                if previous_base <= position_at_change && joined_at == position_later
        ),
        "{joined:?}"
    );
    let processed = tempo_factory_handle.processed_inputs();
    assert_eq!(
        processed.len(),
        2,
        "silence, then the tone: {}",
        processed.len()
    );
    let silence_frames = processed[0].len() / CHANNELS as usize;
    assert!(
        silence_frames.abs_diff(frames_in(LATE_AUDIO - position_later)) <= 1,
        "silence up to the audio PTS: {silence_frames} frames"
    );
    assert!(processed[0].iter().all(|sample| *sample == 0.0));
    assert_eq!(processed[1], vec![TONE; PACKET_FRAMES * CHANNELS as usize]);
    assert_eq!(output_factory_handle.create_count(), 1);
}

#[test]
fn audio_track_switch_while_audio_is_held_drops_the_held_packet_and_starts_a_new_segment() {
    let (mut session, output_factory_handle, [first_track, second_track]) =
        playing_fake_media_with_held_audio(Duration::from_secs(1));
    assert_eq!(
        session.pipeline.selected_audio_track_id(),
        Some(first_track)
    );

    let outcome = session
        .switch_selected_audio_track(second_track)
        .expect("switch to a valid audio track");

    assert!(matches!(
        outcome,
        crate::session::audio_track_switch::AudioTrackSwitchOutcome::Switched(_)
    ));
    assert_eq!(
        session.pipeline.selected_audio_track_id(),
        Some(second_track)
    );
    assert_eq!(
        session.pipeline.front_pending_audio_packet_start(),
        None,
        "the held packet of the old track is gone"
    );
    assert!(session.pipeline.audio_segment_awaits_first_pcm());
    assert_eq!(output_factory_handle.create_count(), 0);
    assert_eq!(session.snapshot().last_error, None);
}

#[test]
fn held_audio_wakes_the_worker_at_its_pts_when_no_frame_is_ahead() {
    // Видео кончилось раньше звука: кадров впереди нет, будить worker к звуку больше некому.
    let (mut session, _output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    enqueue_audio_packet(&mut session, LATE_AUDIO);
    let now = Instant::now();
    run_presentation_for(&mut session, Duration::from_millis(1_900), now);

    let plan = session.worker_wakeup_plan(
        now,
        &PlayerTickConfig::default(),
        Duration::from_millis(5),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, crate::WorkerWakeupReason::PipelineWorkReady);
    let delay = plan.delay.expect("a deadline");
    assert!(
        delay.abs_diff(Duration::from_millis(100)) <= Duration::from_millis(2),
        "wake exactly when the presentation reaches the audio: {delay:?}"
    );
}
