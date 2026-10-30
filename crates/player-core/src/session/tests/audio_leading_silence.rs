//! Звук начинается позже базы часов: тишина перед первым PCM отрезка (сессия 03e моста).
//!
//! Проверяется то, что услышит зритель: PCM в output-е. Первый sample пустого output-а
//! звучит в базе часов, поэтому число нулевых frames перед звуком — это и есть время,
//! на которое звук отложен относительно базы.

use std::sync::Arc;

use super::audio_packet_window::RecordingPcmDecoder;
use super::audio_runtime::FakeTempoFactory;
use super::test_support::*;
use super::*;
use crate::PlaybackRate;
use crate::session::audio_runtime::leading_silence::AudioSegmentStartPlan;

/// Частота PCM тестового звука.
const SAMPLE_RATE: u32 = 48_000;

/// Каналов в тестовом PCM.
const CHANNELS: u32 = 2;

/// PCM frames одного decoded packet-а (20 мс при 48 кГц).
const PACKET_FRAMES: usize = 960;

/// Длительность PCM одного decoded packet-а.
const PACKET_DURATION: Duration = Duration::from_millis(20);

/// Значение каждого sample-а decoded packet-а: заведомо не тишина.
const TONE: f32 = 0.5;

/// Выбранная аудиодорожка.
const AUDIO_TRACK: TrackId = TrackId::new(2);

/// Выбранная видеодорожка.
const VIDEO_TRACK: TrackId = TrackId::new(1);

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

/// Session с выбранными звуком (decoder готов) и, по желанию, видео.
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

/// Есть ли в media выбранное видео.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedVideo {
    /// A/V media.
    Selected,
    /// Audio-only media.
    None,
}

/// Подаёт один audio packet текущего поколения.
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

/// Всего нулевых PCM frames в записанном звуке.
fn silent_frames(pcm: &[f32]) -> usize {
    let (stereo_frames, _) = pcm.as_chunks::<{ CHANNELS as usize }>();
    stereo_frames
        .iter()
        .filter(|frame| frame.iter().all(|sample| *sample == 0.0))
        .count()
}

/// Сколько PCM frames записано всего.
fn written_frames(pcm: &[f32]) -> usize {
    pcm.len() / CHANNELS as usize
}

#[test]
fn late_first_audio_with_video_is_preceded_by_silence_of_the_gap() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    let audio_start = Duration::from_millis(400);

    feed_audio_packet(&mut session, audio_start);

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        leading_silence_frames(&pcm),
        frames_in(audio_start),
        "the first tone sample must play 400 ms after the clock base"
    );
    assert_eq!(
        written_frames(&pcm),
        frames_in(audio_start) + PACKET_FRAMES,
        "silence plus exactly one decoded packet"
    );
    assert_eq!(session.snapshot().last_error, None);
}

#[test]
fn leading_silence_is_not_counted_as_written_audio() {
    let (mut session, _output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    let audio_start = Duration::from_millis(400);

    feed_audio_packet(&mut session, audio_start);

    // Конец звука для `audio_gap.rs` меряется по PTS packet-а: тишина его не сдвигает.
    assert_eq!(
        session.pipeline.written_audio_end(),
        Some(audio_start + PACKET_DURATION)
    );
}

#[test]
fn leading_silence_does_not_touch_video_or_the_clock_base() {
    let (mut session, _output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    let video_packets_before = session.pipeline.pending_video_packet_len();

    feed_audio_packet(&mut session, Duration::from_millis(400));

    assert_eq!(
        session.pipeline.pending_video_packet_len(),
        video_packets_before
    );
    assert!(!session.pipeline.has_present_video_frame());
    assert!(session.pipeline.has_selected_video_track());
    assert_eq!(
        session.pipeline.media_clock_base(),
        Duration::ZERO,
        "with video the clock base stays where playback starts"
    );
}

#[test]
fn following_packets_of_the_segment_are_never_padded() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    let audio_start = Duration::from_millis(400);

    feed_audio_packet(&mut session, audio_start);
    feed_audio_packet(&mut session, audio_start + PACKET_DURATION);
    // Разрыв посреди отрезка — не дело стартовой политики (`audio_gap.rs`, отдельная задача).
    feed_audio_packet(&mut session, Duration::from_millis(700));

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(silent_frames(&pcm), frames_in(audio_start));
    assert_eq!(
        written_frames(&pcm),
        frames_in(audio_start) + 3 * PACKET_FRAMES
    );
}

#[test]
fn gap_within_tolerance_writes_audio_immediately() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);

    feed_audio_packet(&mut session, Duration::from_millis(15));

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(leading_silence_frames(&pcm), 0);
    assert_eq!(written_frames(&pcm), PACKET_FRAMES);
}

#[test]
fn gap_above_the_silence_cap_keeps_writing_at_the_clock_base() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    let audio_start = Duration::from_secs(2);

    assert_eq!(
        session.plan_audio_segment_start(audio_start, SAMPLE_RATE),
        AudioSegmentStartPlan::GapAboveSilenceCap { gap: audio_start }
    );
    feed_audio_packet(&mut session, audio_start);

    // Текущее поведение (решение владельца 03e): тишины нет, звук пишется сразу.
    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(leading_silence_frames(&pcm), 0);
    assert_eq!(written_frames(&pcm), PACKET_FRAMES);
    assert_eq!(
        session.pipeline.written_audio_end(),
        Some(audio_start + PACKET_DURATION)
    );
    assert_eq!(session.snapshot().last_error, None);
}

#[test]
fn silence_is_measured_from_the_clock_base_not_from_zero() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    let clock_base = Duration::from_secs(5);
    session.pipeline.set_media_clock_base(clock_base);

    feed_audio_packet(&mut session, clock_base + Duration::from_millis(200));

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        leading_silence_frames(&pcm),
        frames_in(Duration::from_millis(200))
    );
}

#[test]
fn clock_reanchor_after_the_first_pcm_does_not_restart_the_segment() {
    // Seek commit, смена скорости и пересоздание output-а перепривязывают часы, когда в
    // output уже лежит звук отрезка: следующий packet ложится следом, а не в базу.
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    for packet_index in 0..10_u32 {
        feed_audio_packet(&mut session, PACKET_DURATION * packet_index);
    }
    session.pipeline.set_media_clock_base(Duration::ZERO);
    assert_eq!(
        session.pipeline.written_audio_end(),
        None,
        "the reanchor forgets the gap reference: it alone cannot tell a segment start"
    );

    feed_audio_packet(&mut session, PACKET_DURATION * 10);

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        silent_frames(&pcm),
        0,
        "no silence inside a running segment"
    );
    assert_eq!(written_frames(&pcm), 11 * PACKET_FRAMES);
}

#[test]
fn seek_buffer_clear_starts_a_new_segment_measured_from_the_seek_base() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    feed_audio_packet(&mut session, Duration::ZERO);
    let written_before_seek = written_frames(&written_pcm(&output_factory_handle));
    let seek_target = Duration::from_secs(3);

    // Тот же порядок, что у старта seek-а: новое поколение, очистка output-а, база = цель.
    let generation = session.pipeline.begin_seek_generation();
    let _clear_ack = session.pipeline.clear_audio_output_for_seek(generation);
    session.pipeline.reset_clocks_for_seek(seek_target);
    feed_audio_packet(&mut session, seek_target + Duration::from_millis(300));

    let pcm = written_pcm(&output_factory_handle);
    let after_seek = &pcm[written_before_seek * CHANNELS as usize..];
    assert_eq!(
        leading_silence_frames(after_seek),
        frames_in(Duration::from_millis(300))
    );
    assert_eq!(
        written_frames(after_seek),
        frames_in(Duration::from_millis(300)) + PACKET_FRAMES
    );
}

#[test]
fn audio_track_switch_starts_a_new_segment_on_the_new_output() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    feed_audio_packet(&mut session, Duration::ZERO);
    let switch_position = Duration::from_secs(1);

    // Pipeline-часть переключения дорожки (`audio_track_switch.rs`): старый output уходит.
    session.pipeline.replace_selected_audio_track(
        AUDIO_TRACK,
        audio_core::AudioDecoderConfig::new(AUDIO_TRACK.get(), "aac", SAMPLE_RATE, CHANNELS),
    );
    session.pipeline.install_audio_decoder(tone_decoder());
    session.pipeline.set_media_clock_base(switch_position);
    feed_audio_packet(&mut session, switch_position + Duration::from_millis(250));

    assert_eq!(output_factory_handle.create_count(), 2, "a fresh output");
    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        leading_silence_frames(&pcm),
        frames_in(Duration::from_millis(250))
    );
}

#[test]
fn audio_only_starts_at_the_first_sample_and_moves_the_clock_there() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::None);
    let audio_start = Duration::from_millis(400);

    feed_audio_packet(&mut session, audio_start);

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        leading_silence_frames(&pcm),
        0,
        "nothing to watch: no silence"
    );
    assert_eq!(written_frames(&pcm), PACKET_FRAMES);
    assert_eq!(
        session.pipeline.media_clock_base(),
        audio_start,
        "the first sample plays at its own time on the timeline"
    );
    assert_eq!(session.snapshot().current_position, audio_start);
    assert_eq!(
        session.pipeline.written_audio_end(),
        Some(audio_start + PACKET_DURATION)
    );
}

#[test]
fn audio_only_seek_leaves_the_clock_base_to_the_seek_commit() {
    let audio_track = fake_track(AUDIO_TRACK.get(), TrackKind::Audio);
    let seek_target = Duration::from_secs(2);
    let demuxer = scripted_seek_demuxer(
        vec![audio_track.clone()],
        seek_target,
        seek_target,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![audio_track], demuxer);
    harness
        .session
        .pipeline
        .install_audio_decoder(tone_decoder());
    harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_duration(seek_target),
        )))
        .expect("seek command");
    assert!(harness.session.has_active_seek_commit());
    let audio_start = seek_target + Duration::from_millis(300);

    assert_eq!(
        harness
            .session
            .plan_audio_segment_start(audio_start, SAMPLE_RATE),
        AudioSegmentStartPlan::AudioOnlySeekKeepsClockBase {
            gap: Duration::from_millis(300)
        }
    );
}

#[test]
fn non_normal_rate_stretches_leading_silence_with_the_audio() {
    let (output_factory, _output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);
    session
        .pipeline
        .select_video_track(VIDEO_TRACK, VideoDecodeRequirement::new(VideoCodec::H264));
    session.pipeline.select_audio_track(AUDIO_TRACK);
    session.pipeline.install_audio_decoder(tone_decoder());
    // Скорость 2x принимается только при известном формате PCM: первый отрезок звука.
    feed_audio_packet(&mut session, Duration::ZERO);
    session.set_playback_state(PlaybackState::Paused);
    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(
                PlaybackRate::new(2.0).expect("valid 2x rate"),
            ))
            .expect("rate command"),
        PlayerCommandOutcome::Applied
    );
    // Новый отрезок (как после seek-а) со звуком на 400 мс позже базы.
    let generation = session.pipeline.begin_seek_generation();
    let _clear_ack = session.pipeline.clear_audio_output_for_seek(generation);
    session.pipeline.reset_clocks_for_seek(Duration::ZERO);

    feed_audio_packet(&mut session, Duration::from_millis(400));

    // Тишина идёт в tempo processor раньше звука: при 2x он растягивает её вместе со звуком.
    let processed = tempo_factory_handle.processed_inputs();
    assert_eq!(processed.len(), 2, "silence and the packet: {processed:?}");
    assert_eq!(
        processed[0],
        vec![0.0; frames_in(Duration::from_millis(400)) * CHANNELS as usize]
    );
    assert_eq!(processed[1], vec![TONE; PACKET_FRAMES * CHANNELS as usize]);
}

#[test]
fn new_generation_without_a_ring_clear_does_not_restart_the_segment() {
    // Выход из scrub без цели (`invalidate_in_flight_scrub_outputs_after_exit`) меняет
    // поколение, но ring и база остаются: следующий packet ложится следом за звуком.
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    for packet_index in 0..10_u32 {
        feed_audio_packet(&mut session, PACKET_DURATION * packet_index);
    }

    session.invalidate_in_flight_scrub_outputs_after_exit("leading silence test");
    feed_audio_packet(&mut session, PACKET_DURATION * 10);

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        silent_frames(&pcm),
        0,
        "no silence in the middle of the stream"
    );
    assert_eq!(written_frames(&pcm), 11 * PACKET_FRAMES);
    assert_eq!(session.snapshot().last_error, None);
}

#[test]
fn track_list_reset_starts_a_new_segment_on_a_fresh_output() {
    // `TracksChanged`/`ResetRequired`: pipeline удаляет output, поэтому следующий PCM
    // ляжет в точку привязки часов нового output-а — это настоящее начало отрезка.
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::None);
    feed_audio_packet(&mut session, Duration::ZERO);
    assert!(!session.pipeline.audio_segment_awaits_first_pcm());

    session.handle_demux_track_list_update(media_core::DemuxTrackListUpdate::new(
        vec![fake_track(AUDIO_TRACK.get(), TrackKind::Audio)],
        Some(Duration::from_secs(30)),
    ));

    assert!(!session.pipeline.has_audio_output());
    assert!(session.pipeline.audio_segment_awaits_first_pcm());
    session.pipeline.select_audio_track(AUDIO_TRACK);
    session.pipeline.install_audio_decoder(tone_decoder());
    let audio_start = Duration::from_millis(300);
    feed_audio_packet(&mut session, audio_start);
    assert_eq!(output_factory_handle.create_count(), 2, "a fresh output");
    assert_eq!(
        leading_silence_frames(&written_pcm(&output_factory_handle)),
        0
    );
    assert_eq!(session.pipeline.media_clock_base(), audio_start);
}

/// Output, который принимает PCM, но не может очистить ring под seek.
struct ClearFailingOutput;

/// Часы такого output-а стоят в нуле.
struct StoppedClock;

impl PlayerAudioClock for StoppedClock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }

    fn output_timing(&self) -> audio_core::AudioOutputClockTiming {
        audio_core::AudioOutputClockTiming::new(Duration::ZERO, Duration::ZERO)
    }

    fn reset(&self) {}

    fn underrun_callbacks(&self) -> u64 {
        0
    }
}

impl PlayerAudioOutput for ClearFailingOutput {
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: crate::AudioOutputWriteIntent,
    ) -> std::result::Result<crate::AudioOutputWriteReport, crate::AudioOutputWriteError> {
        let frames = samples.len() / CHANNELS as usize;
        Ok(crate::AudioOutputWriteReport::complete(
            crate::AudioOutputInputFrameCount::new(frames),
            crate::AudioOutputStreamFrameCount::new(frames),
        ))
    }

    fn play(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<audio_core::AudioOutputClockTiming> {
        Ok(StoppedClock.output_timing())
    }

    fn clear_buffer_for_seek(&mut self, _generation: u64) -> anyhow::Result<u64> {
        anyhow::bail!("scripted ring clear failure")
    }

    fn set_volume(&mut self, _volume: f32) {}

    fn buffer_level_ms(&self) -> f64 {
        0.0
    }

    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        Arc::new(StoppedClock)
    }
}

#[test]
fn failed_ring_clear_keeps_the_running_segment() {
    let (mut session, _output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    session
        .pipeline
        .install_audio_output_for_tests(Box::new(ClearFailingOutput));
    feed_audio_packet(&mut session, Duration::ZERO);
    assert!(!session.pipeline.audio_segment_awaits_first_pcm());

    let generation = session.pipeline.begin_seek_generation();
    let clear_result = session.pipeline.clear_audio_output_for_seek(generation);

    assert!(matches!(clear_result, Some(Err(_))));
    assert!(
        !session.pipeline.audio_segment_awaits_first_pcm(),
        "old audio is still in the ring: the next PCM does not land at the anchor"
    );
}

#[test]
fn audio_gap_resume_starts_a_new_segment_from_the_resumed_position() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    feed_audio_packet(&mut session, Duration::ZERO);
    let handover = session
        .pipeline
        .hand_over_presentation_clock_for_audio_gap(Instant::now(), PlaybackRate::NORMAL);
    assert!(
        matches!(
            handover,
            crate::pipeline::AudioEndClockHandoverOutcome::HandedOver { .. }
        ),
        "{handover:?}"
    );
    let resumed_at = Duration::from_secs(7);

    let resume = session.pipeline.resume_audio_clock_after_gap(resumed_at);

    assert_eq!(
        resume,
        crate::pipeline::AudioGapResumeOutcome::Resumed { resumed_at }
    );
    assert!(session.pipeline.audio_segment_awaits_first_pcm());
    // Обычно packet после разрыва не позже `resumed_at` (обрезается); если же он позже,
    // его выравнивание меряется от новой базы, а не от старого звука.
    let written_before = written_frames(&written_pcm(&output_factory_handle));
    feed_audio_packet(&mut session, resumed_at + Duration::from_millis(100));
    let pcm = written_pcm(&output_factory_handle);
    let after_resume = &pcm[written_before * CHANNELS as usize..];
    assert_eq!(
        leading_silence_frames(after_resume),
        frames_in(Duration::from_millis(100))
    );
}

#[test]
fn audio_only_gap_above_the_cap_keeps_the_clock_base() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::None);
    // Метка времени «из другой эпохи»: часы не переносятся за шкалу.
    let far_audio_start = Duration::from_secs(3 * 60 * 60);

    feed_audio_packet(&mut session, far_audio_start);

    assert_eq!(session.pipeline.media_clock_base(), Duration::ZERO);
    assert_eq!(session.snapshot().current_position, Duration::ZERO);
    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(leading_silence_frames(&pcm), 0);
    assert_eq!(written_frames(&pcm), PACKET_FRAMES);
}
