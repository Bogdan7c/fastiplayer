//! Звук кончается раньше видео: хвост видео доигрывается по monotonic clock и доходит до `Ended`.
//!
//! Живой T410 (сессия 13b): H.264 720p30, видео 12.07 с, звук обрезан до 10.0 с —
//! картинка замирала на ~10 с, файл не заканчивался, очередь не шла дальше. Seek за
//! последний кадр 12-секундного AV1 (fallback-кадр 11.9 с, звук хвоста пропущен как
//! pre-target) так же навсегда оставался в `Draining`. Причина: audio clock — единственный
//! источник времени показа, а после доигрывания звука он стоит; кадры после конца звука
//! никогда не становились due.

use super::test_support::*;
use super::*;
use crate::PlaybackRate;
use crate::pipeline::PresentationClockSource;

/// Длительность media из живого случая (видео длиннее звука).
const MEDIA_DURATION: Duration = Duration::from_millis(12_070);

/// Конец звуковой дорожки (обрезанное аудио).
const AUDIO_END: Duration = Duration::from_secs(10);

/// Сколько PCM ещё лежит в output-е в момент EOF demux-а.
const AUDIO_TAIL_AT_EOF: Duration = Duration::from_millis(40);

/// Шаг wall-time между worker tick-ами в сценариях.
const TICK_STEP: Duration = Duration::from_millis(50);

/// Верхняя граница tick-ов: 2 с хвоста при шаге 50 мс плюс запас; дальше — зависание.
const MAX_TAIL_TICKS: u32 = 120;

/// Кадры хвоста видео после конца звука; последний — реальный последний кадр файла.
fn video_tail_pts() -> Vec<Duration> {
    [
        10_250_u64, 10_500, 10_750, 11_000, 11_250, 11_500, 11_750, 12_000,
    ]
    .into_iter()
    .map(Duration::from_millis)
    .collect()
}

/// Session с идущим A/V playback-ом, где demux ещё отдаёт только хвост видео.
struct AudioEndsBeforeVideo {
    /// Session под тестом.
    session: PlayerSession,

    /// Fake output: тест управляет уровнем buffer-а и audio clock-ом.
    audio_output: ScriptedAudioOutputHandle,

    /// Fake decoder: каждый отправленный packet сразу становится кадром.
    decoder: SharedFakeVideoDecoderThread,
}

impl AudioEndsBeforeVideo {
    /// Собирает Playing media: звук почти доигран (40 мс до конца), видео ещё 2 с.
    fn start(tail_pts: &[Duration]) -> Self {
        let video_track = fake_track(1, TrackKind::Video);
        let audio_track = fake_track(2, TrackKind::Audio);
        let packets = tail_pts
            .iter()
            .enumerate()
            .map(|(index, pts)| {
                let keyframe = if index == 0 {
                    PacketKeyframe::Keyframe
                } else {
                    PacketKeyframe::NotKeyframe
                };
                fake_video_packet_with_keyframe(video_track.id, *pts, keyframe)
            })
            .collect();
        let demuxer = scripted_seek_demuxer(
            vec![video_track.clone(), audio_track.clone()],
            Duration::ZERO,
            Duration::ZERO,
            packets,
        );
        let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);
        harness.session.set_snapshot_duration(Some(MEDIA_DURATION));
        for (handle, pts) in (900_u64..).zip(tail_pts) {
            harness.decoder.decode_next_packet_as_frame(*pts, handle);
        }

        let audio_output = install_ready_audio_runtime(&mut harness.session, 0.0, None);
        // Audible 9.96 с, submitted до 10.0 с: в DAC ещё 40 мс последнего звука.
        audio_output.set_underlying_output_timing(AUDIO_END - AUDIO_TAIL_AT_EOF, AUDIO_END);
        harness
            .session
            .update_current_position(AUDIO_END - AUDIO_TAIL_AT_EOF);
        harness
            .session
            .dispatch_command(PlayerCommand::Play)
            .expect("Play must reach the session");
        let _start_events = harness.session.take_events();

        Self {
            session: harness.session,
            audio_output,
            decoder: harness.decoder,
        }
    }

    /// Output доиграл весь PCM: audio clock встал ровно на конце звука.
    fn finish_audio_output(&self) {
        self.audio_output
            .set_underlying_output_timing(AUDIO_END, AUDIO_END);
        self.audio_output.set_buffer_level_ms(0.0);
    }

    /// Один worker tick на заданном monotonic моменте.
    fn tick_at(&mut self, now: Instant) -> PlayerTickResult {
        self.session.tick(PlayerTickContext::new(now))
    }

    /// Tick-и до входа в EOF drain (demux дочитал хвост видео до EOF).
    fn tick_until_eof_drain(&mut self, started_at: Instant) {
        for tick_index in 0..MAX_TAIL_TICKS {
            let _tick = self.tick_at(started_at + TICK_STEP * tick_index);
            if self.session.is_eof_draining() {
                return;
            }
        }
        panic!("demux must reach EOF and enter Draining");
    }

    /// Tick-и с шагом [`TICK_STEP`], пока session не дойдёт до `Ended`.
    ///
    /// Возвращает PTS кадров в порядке показа и момент последнего tick-а.
    fn tick_until_ended(&mut self, started_at: Instant) -> (Vec<Duration>, Instant) {
        let mut presented_pts = Vec::new();
        for tick_index in 0..MAX_TAIL_TICKS {
            let now = started_at + TICK_STEP * tick_index;
            let _tick = self.tick_at(now);
            self.record_present_frame(&mut presented_pts);
            if self.session.playback_state() == PlaybackState::Ended {
                return (presented_pts, now);
            }
        }
        panic!(
            "video tail must reach Ended; state={:?} position={:?} presented={presented_pts:?} \
             queued_frames={}",
            self.session.playback_state(),
            self.session.snapshot().current_position,
            self.session.pipeline.video_present_queue_len(),
        );
    }

    /// Запоминает новый present frame (каждый PTS — один раз).
    fn record_present_frame(&self, presented_pts: &mut Vec<Duration>) {
        let Some(pts) = self
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts)
        else {
            return;
        };
        if presented_pts.last() != Some(&pts) {
            presented_pts.push(pts);
        }
    }

    /// Доводит сценарий до момента сразу после передачи времени хвосту видео.
    fn advance_to_handed_over_tail(&mut self, started_at: Instant) -> Instant {
        self.tick_until_eof_drain(started_at);
        self.finish_audio_output();
        let handover_at = started_at + TICK_STEP * MAX_TAIL_TICKS;
        let _handover_tick = self.tick_at(handover_at);
        assert_eq!(
            self.session.pipeline.presentation_clock_source(),
            PresentationClockSource::MonotonicAfterAudioEnd,
            "drained audio with a remaining video tail must hand over the clock"
        );
        handover_at
    }
}

/// Проверяет, что событие `Ended` опубликовано (по нему playlist переходит дальше).
fn assert_ended_event_published(session: &mut PlayerSession) {
    let events = session.take_events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            PlayerEvent::PlaybackStateChanged(PlaybackState::Ended)
        )),
        "Ended event is the terminal edge for playlist auto-advance: {events:?}"
    );
}

#[test]
fn audio_ending_before_video_plays_video_tail_and_reaches_ended() {
    let tail_pts = video_tail_pts();
    let mut scenario = AudioEndsBeforeVideo::start(&tail_pts);
    let started_at = Instant::now();

    scenario.tick_until_eof_drain(started_at);
    assert_eq!(
        scenario.session.pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput,
        "while audio still plays, the audio clock keeps driving presentation"
    );
    scenario.finish_audio_output();
    let tail_started_at = started_at + TICK_STEP * MAX_TAIL_TICKS;
    let (presented_pts, ended_at) = scenario.tick_until_ended(tail_started_at);

    assert_eq!(
        presented_pts, tail_pts,
        "every video frame after the audio end must be presented in order"
    );
    assert_eq!(
        scenario.decoder.sent_packets().len(),
        tail_pts.len(),
        "the whole video tail must pass the decoder boundary"
    );
    // Хвост идёт в темпе media (2 с при 1x), а не «по кадру на tick»: иначе картинка
    // пролетела бы последние секунды файла.
    let tail_wall_time = ended_at.saturating_duration_since(tail_started_at);
    assert!(
        tail_wall_time >= Duration::from_millis(1_500),
        "2 s video tail must be paced by the media clock, finished after {tail_wall_time:?}"
    );
    assert_eq!(scenario.session.snapshot().current_position, MEDIA_DURATION);
    assert_ended_event_published(&mut scenario.session);
}

#[test]
fn seek_past_last_video_frame_with_drained_audio_reaches_ended() {
    let duration = Duration::from_secs(12);
    let fallback_pts = Duration::from_millis(11_900);
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    session.set_snapshot_duration(Some(duration));
    let decoder = SharedFakeVideoDecoderThread::new();
    session.pipeline.set_video_decoder_thread(decoder.clone());
    // Хвост звука 11.9–12.0 с пропущен как pre-target: output сразу пуст.
    let _audio_output = install_ready_audio_runtime(&mut session, 0.0, None);
    session.update_current_position(Duration::from_secs(5));
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(12),
        )))
        .unwrap();
    decoder.set_eof_drain_state(video_core::VideoDecoderEndOfStreamDrainState::Drained {
        generation: session.pipeline.seek_generation(),
    });

    // Demux дошёл до EOF без кадра на target: seek закрывается последним кадром до него.
    session.enter_eof_drain();
    let fallback_frame = decoded_frame_for_current_seek_generation(&session, fallback_pts, 1_190);
    session.replace_seek_preroll_fallback_frame(fallback_frame);
    let started_at = Instant::now();
    let commit_tick = session.tick(PlayerTickContext::new(started_at));
    assert_eq!(commit_tick.video_frames_presented, 1);
    assert!(session.seek_commit().is_none(), "eof-fallback-frame commit");
    assert_eq!(session.snapshot().current_position, fallback_pts);

    // Как в живой трассе: после commit в очереди остались два последних кадра файла.
    for (handle, pts_ms) in [(1_191_u64, 11_933_u64), (1_192, 11_967)] {
        let frame = decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_millis(pts_ms),
            handle,
        );
        session.pipeline.enqueue_queued_video_frame(frame);
    }

    let mut reached_ended = false;
    let mut last_presented = None;
    for tick_index in 1..=MAX_TAIL_TICKS {
        let _tick = session.tick(PlayerTickContext::new(started_at + TICK_STEP * tick_index));
        last_presented = session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts);
        if session.playback_state() == PlaybackState::Ended {
            reached_ended = true;
            break;
        }
    }

    assert!(
        reached_ended,
        "seek past the last frame must finish the file; state={:?} queued={}",
        session.playback_state(),
        session.pipeline.video_present_queue_len()
    );
    assert_eq!(
        last_presented,
        Some(Duration::from_millis(11_967)),
        "the remaining tail frames must be shown before Ended"
    );
    assert_eq!(session.snapshot().current_position, duration);
    assert_ended_event_published(&mut session);
}

#[test]
fn pause_during_video_tail_freezes_position_and_resume_reaches_ended() {
    let mut scenario = AudioEndsBeforeVideo::start(&video_tail_pts());
    // Pause сама читает `Instant::now()`, поэтому сценарий сдвинут в прошлое так, что
    // tick 300 мс хвоста приходится примерно на «сейчас».
    let tail_offset = TICK_STEP * MAX_TAIL_TICKS + Duration::from_millis(300);
    let started_at = Instant::now()
        .checked_sub(tail_offset)
        .expect("test clock must have enough monotonic headroom");
    let handover_at = scenario.advance_to_handed_over_tail(started_at);
    // 300 мс хвоста после конца звука: позиция ушла за 10.0 с по monotonic clock.
    let tail_tick_at = handover_at + Duration::from_millis(300);
    let _tail_tick = scenario.tick_at(tail_tick_at);
    let tail_position = scenario.session.snapshot().current_position;
    assert_eq!(tail_position, AUDIO_END + Duration::from_millis(300));

    scenario
        .session
        .dispatch_command(PlayerCommand::Pause)
        .unwrap();
    for paused_tick_index in 1..=4 {
        let _paused_tick = scenario.tick_at(tail_tick_at + Duration::from_secs(paused_tick_index));
    }

    assert_eq!(scenario.session.playback_state(), PlaybackState::Paused);
    assert!(
        scenario.session.snapshot().current_position >= tail_position,
        "Pause must not jump back to the stopped audio clock (audio end)"
    );
    let paused_position = scenario.session.snapshot().current_position;
    assert_eq!(
        scenario
            .session
            .presentation_clock_position_at(tail_tick_at + Duration::from_secs(10)),
        paused_position,
        "paused video tail must not advance with wall time"
    );

    scenario
        .session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();
    let (_presented, _ended_at) = scenario.tick_until_ended(tail_tick_at + Duration::from_secs(5));
    assert_eq!(scenario.session.snapshot().current_position, MEDIA_DURATION);
    assert_ended_event_published(&mut scenario.session);
}

#[test]
fn video_tail_after_audio_end_advances_at_current_playback_rate() {
    let mut scenario = AudioEndsBeforeVideo::start(&video_tail_pts());
    // Rate command boundary проверяется своими тестами; здесь важно, что хвост берёт
    // текущую скорость, поэтому задаём её до EOF напрямую в snapshot.
    scenario.session.snapshot.playback_rate =
        PlaybackRate::new(2.0).expect("2x playback rate must validate");
    let handover_at = scenario.advance_to_handed_over_tail(Instant::now());

    let _tail_tick = scenario.tick_at(handover_at + Duration::from_millis(250));

    assert_eq!(
        scenario.session.snapshot().current_position,
        AUDIO_END + Duration::from_millis(500),
        "250 ms of wall time at 2x must move the video tail by 500 ms of media time"
    );
    let (presented_pts, ended_at) =
        scenario.tick_until_ended(handover_at + Duration::from_millis(300));
    assert_eq!(presented_pts.last(), Some(&Duration::from_secs(12)));
    let tail_wall_time = ended_at.saturating_duration_since(handover_at);
    assert!(
        tail_wall_time >= Duration::from_millis(700)
            && tail_wall_time < Duration::from_millis(1_500),
        "2 s of media tail at 2x must finish in about 1 s of wall time, took {tail_wall_time:?}"
    );
}

#[test]
fn seek_backwards_from_video_tail_returns_presentation_to_audio_clock() {
    let mut scenario = AudioEndsBeforeVideo::start(&video_tail_pts());
    let handover_at = scenario.advance_to_handed_over_tail(Instant::now());
    let seek_target = Duration::from_secs(6);

    scenario
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_duration(seek_target),
        )))
        .unwrap();
    assert_eq!(
        scenario.session.pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput,
        "a seek is a timeline discontinuity and must cancel the handover"
    );

    // Seek приземляется штатно: целевой кадр показан, звук снова буферизован.
    scenario.audio_output.set_buffer_level_ms(80.0);
    present_frame_for_current_seek_generation(
        &mut scenario.session,
        Duration::from_millis(6_016),
        950,
    );
    let seek_commit = scenario
        .session
        .seek_commit()
        .expect("backward seek must be active until audio commits");
    scenario.session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert!(scenario.session.seek_commit().is_none());
    assert_eq!(scenario.session.playback_state(), PlaybackState::Playing);
    assert_eq!(scenario.session.snapshot().current_position, seek_target);
    assert_eq!(
        scenario.session.pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput
    );
    // Время снова ведёт звук: wall-time без audio progress позицию не двигает...
    let far_future = handover_at + Duration::from_secs(30);
    assert_eq!(
        scenario.session.presentation_clock_position_at(far_future),
        seek_target
    );
    // ...а audio progress двигает.
    let audio_progress = Duration::from_millis(120);
    let audio_clock_now = scenario.audio_output.audible_output_position();
    scenario.audio_output.set_underlying_output_timing(
        audio_clock_now + audio_progress,
        audio_clock_now + audio_progress + Duration::from_millis(80),
    );
    assert_eq!(
        scenario.session.presentation_clock_position_at(far_future),
        seek_target + audio_progress
    );
    assert_ne!(scenario.session.playback_state(), PlaybackState::Ended);
}

#[test]
fn replay_after_video_tail_ended_does_not_inherit_handover() {
    let mut scenario = AudioEndsBeforeVideo::start(&video_tail_pts());
    let handover_at = scenario.advance_to_handed_over_tail(Instant::now());
    let (_presented, ended_at) = scenario.tick_until_ended(handover_at + TICK_STEP);
    let _events = scenario.session.take_events();

    scenario
        .session
        .dispatch_command(PlayerCommand::TogglePlayback)
        .unwrap();

    let replay_seek = scenario
        .session
        .seek_commit()
        .expect("toggle after Ended must start the replay seek");
    assert_eq!(replay_seek.target_position, MediaTime::ZERO);
    assert_eq!(scenario.session.playback_state(), PlaybackState::Seeking);
    assert_eq!(
        scenario.session.pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput,
        "replay starts a new timeline: the audio clock drives it again"
    );
    let replay_position = scenario.session.presentation_clock_position_at(ended_at);
    assert_eq!(
        scenario
            .session
            .presentation_clock_position_at(ended_at + Duration::from_secs(5)),
        replay_position,
        "no leftover monotonic tail clock may move the replay position"
    );
}

#[test]
fn audio_and_video_ending_together_finish_without_clock_handover() {
    let last_frame_pts = Duration::from_millis(9_967);
    let mut scenario = AudioEndsBeforeVideo::start(&[last_frame_pts]);
    let started_at = Instant::now();
    scenario.tick_until_eof_drain(started_at);

    scenario.finish_audio_output();
    let mut presented_pts = Vec::new();
    let terminal_tick_at = started_at + TICK_STEP * MAX_TAIL_TICKS;
    let _terminal_tick = scenario.tick_at(terminal_tick_at);
    scenario.record_present_frame(&mut presented_pts);

    assert_eq!(
        scenario.session.playback_state(),
        PlaybackState::Ended,
        "the last frame is due by the audio clock: the drain ends in the same tick"
    );
    assert_eq!(presented_pts, vec![last_frame_pts]);
    assert_eq!(
        scenario.session.pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput,
        "files whose audio and video end together must never switch clocks"
    );
    assert_ended_event_published(&mut scenario.session);
}
