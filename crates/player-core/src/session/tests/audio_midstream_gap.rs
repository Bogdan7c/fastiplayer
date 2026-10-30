//! Дыра в звуке посреди отрезка короче порога разрыва: тишина вместо склейки (D3, 03e).
//!
//! Проверяется то, что услышит зритель: PCM в output-е. Звук отрезка лежит в output-е
//! подряд, поэтому число нулевых frames между packet-ами — это и есть время, на которое
//! звук после дыры отложен относительно звука до неё. Сквозной сценарий с настоящими
//! декодерами — `pushed-media-source/tests/audio_midstream_gap_to_frame.rs`.

use std::sync::Arc;

use bytes::Bytes;
use media_core::TimelineMode;

use super::audio_runtime::FakeTempoFactory;
use super::test_support::*;
use super::*;
use crate::session::audio_gap::PendingAudioOutputAdmission;
use crate::session::audio_gap::scheduled_audio_start::SegmentStartAudioSchedule;
use crate::session::audio_runtime::leading_silence::AudioSegmentStartPlan;
use crate::session::audio_runtime::mid_stream_gap::MidStreamGapPlan;
use crate::{PendingAudioPacket, PlaybackRate};

/// Частота PCM тестового звука.
const SAMPLE_RATE: u32 = 48_000;

/// Каналов в тестовом PCM.
const CHANNELS: u32 = 2;

/// PCM frames одного decoded packet-а: AAC-кадр (21,33 мс при 48 кГц).
const PACKET_FRAMES: usize = 1_024;

/// Значение каждого sample-а decoded packet-а: заведомо не тишина.
const TONE: f32 = 0.5;

/// Payload packet-а, который decoder декодирует в тон.
const TONE_PACKET: &[u8] = b"aac-frame";

/// Payload packet-а, который decoder не может декодировать.
const CORRUPT_PACKET: &[u8] = b"corrupt-aac-frame";

/// Выбранная аудиодорожка.
const AUDIO_TRACK: TrackId = TrackId::new(2);

/// Выбранная видеодорожка.
const VIDEO_TRACK: TrackId = TrackId::new(1);

/// Ёмкость ring-а вывода в тесте переполнения (как у `audio::output`, 2 с).
const RING_CAPACITY_FRAMES: usize = 2 * SAMPLE_RATE as usize;

/// Media-время начала packet-а с номером `index` в непрерывном потоке AAC-кадров.
fn packet_start(index: u64) -> Duration {
    Duration::from_nanos(index * PACKET_FRAMES as u64 * 1_000_000_000 / u64::from(SAMPLE_RATE))
}

/// PCM frames в длительности (тестовая частота, с округлением до ближайшего).
fn frames_in(duration: Duration) -> usize {
    let scaled = duration.as_nanos() * u128::from(SAMPLE_RATE) + 500_000_000;
    usize::try_from(scaled / 1_000_000_000).expect("test durations fit usize")
}

/// Decoder, который на каждый packet отдаёт AAC-кадр тона, а «битый» packet отвергает.
struct ToneDecoder;

impl audio_core::AudioDecoder for ToneDecoder {
    fn decode(&mut self, packet: &audio_core::EncodedAudioPacket<'_>) -> anyhow::Result<Vec<f32>> {
        if packet.data() == CORRUPT_PACKET {
            anyhow::bail!("scripted corrupt AAC frame");
        }
        Ok(vec![TONE; PACKET_FRAMES * CHANNELS as usize])
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn channels(&self) -> u32 {
        CHANNELS
    }

    fn channel_layout(&self) -> Option<audio_core::AudioChannelLayout> {
        Some(audio_core::AudioChannelLayout::stereo())
    }
}

/// Фабрика decoder-ов тона (для смены кодека посреди потока).
struct ToneDecoderFactory;

impl audio_core::AudioDecoderFactory for ToneDecoderFactory {
    fn create_decoder(
        &self,
        _config: audio_core::AudioDecoderConfig,
    ) -> anyhow::Result<audio_core::AudioDecoderHandle> {
        Ok(Box::new(ToneDecoder))
    }
}

/// Есть ли в media выбранное видео.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedVideo {
    /// A/V media.
    Selected,
    /// Audio-only media.
    None,
}

/// Выбирает дорожки и ставит decoder тона.
fn select_tracks(session: &mut PlayerSession, selected_video: SelectedVideo) {
    if selected_video == SelectedVideo::Selected {
        session
            .pipeline
            .select_video_track(VIDEO_TRACK, VideoDecodeRequirement::new(VideoCodec::H264));
    }
    session.pipeline.select_audio_track(AUDIO_TRACK);
    session
        .pipeline
        .install_audio_decoder(Box::new(ToneDecoder));
}

/// Session с выбранным звуком и, по желанию, видео; output создаётся первым PCM.
fn session_with_audio(
    selected_video: SelectedVideo,
) -> (PlayerSession, ScriptedAudioOutputFactoryHandle) {
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(output_factory);
    select_tracks(&mut session, selected_video);
    (session, output_factory_handle)
}

/// Отдаёт один audio packet прямо в decode/output (минуя очередь и её удержания).
fn feed_packet(session: &mut PlayerSession, packet_pts: Duration, payload: &'static [u8]) {
    let generation = session.pipeline.seek_generation();
    session.process_audio_packet(AUDIO_TRACK, packet_pts, None, None, generation, payload);
}

/// Отдаёт packets тона с номерами `indexes` (непрерывный поток AAC-кадров).
fn feed_tone_packets(session: &mut PlayerSession, indexes: std::ops::Range<u64>) {
    for index in indexes {
        feed_packet(session, packet_start(index), TONE_PACKET);
    }
}

/// Кладёт audio packet тона текущего поколения в очередь (как demux).
fn enqueue_tone_packet(session: &mut PlayerSession, packet_pts: Duration) {
    let generation = session.pipeline.seek_generation();
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
            AUDIO_TRACK,
            packet_pts,
            None,
            None,
            generation,
            Bytes::from_static(TONE_PACKET),
        ));
}

/// PCM, записанный в последний созданный output.
fn written_pcm(output_factory_handle: &ScriptedAudioOutputFactoryHandle) -> Vec<f32> {
    output_factory_handle
        .last_output_handle()
        .expect("audio output must be created by the first PCM")
        .written_samples()
}

/// Всего нулевых PCM frames в записанном звуке.
fn silent_frames(pcm: &[f32]) -> usize {
    let (stereo_frames, _) = pcm.as_chunks::<{ CHANNELS as usize }>();
    stereo_frames
        .iter()
        .filter(|frame| frame.iter().all(|sample| *sample == 0.0))
        .count()
}

/// Номер PCM frame-а, с которого начинается первая тишина, и её длина в frames.
fn first_silence(pcm: &[f32]) -> Option<(usize, usize)> {
    let (stereo_frames, _) = pcm.as_chunks::<{ CHANNELS as usize }>();
    let is_silent = |frame: &[f32; CHANNELS as usize]| frame.iter().all(|sample| *sample == 0.0);
    let start = stereo_frames.iter().position(is_silent)?;
    let length = stereo_frames[start..]
        .iter()
        .take_while(|frame| is_silent(frame))
        .count();
    Some((start, length))
}

/// Сколько PCM frames записано всего.
fn written_frames(pcm: &[f32]) -> usize {
    pcm.len() / CHANNELS as usize
}

/// Session с tempo processor-ом, где после первого packet-а включена скорость `rate`.
///
/// Скорость принимается только при известном формате PCM, поэтому первый packet
/// пишется на 1x (как в `audio_leading_silence.rs`).
fn session_at_rate(
    rate: f32,
) -> (
    PlayerSession,
    ScriptedAudioOutputFactoryHandle,
    super::audio_runtime::FakeTempoFactoryHandle,
) {
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);
    select_tracks(&mut session, SelectedVideo::Selected);
    feed_tone_packets(&mut session, 0..1);
    session.set_playback_state(PlaybackState::Paused);
    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(
                PlaybackRate::new(rate).expect("valid test rate"),
            ))
            .expect("rate command"),
        PlayerCommandOutcome::Applied
    );
    (session, output_factory_handle, tempo_factory_handle)
}

#[test]
fn hole_mid_segment_is_closed_by_silence_of_its_length() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    feed_tone_packets(&mut session, 0..5);
    let hole = Duration::from_millis(300);

    feed_packet(&mut session, packet_start(5) + hole, TONE_PACKET);

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        first_silence(&pcm),
        Some((5 * PACKET_FRAMES, frames_in(hole))),
        "the tone after the hole plays 300 ms after the tone before it"
    );
    assert_eq!(written_frames(&pcm), 6 * PACKET_FRAMES + frames_in(hole));
    assert_eq!(session.snapshot().last_error, None);
}

#[test]
fn silence_is_not_counted_in_the_cursor_or_the_written_audio_end() {
    let (mut session, _output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    feed_tone_packets(&mut session, 0..5);
    let after_hole = packet_start(5) + Duration::from_millis(300);

    feed_packet(&mut session, after_hole, TONE_PACKET);

    // Оба конца меряются по PTS и decoded frames packet-а: тишина их не сдвигает.
    let packet_end = after_hole + packet_start(1);
    assert_eq!(
        session.pipeline.expected_next_audio_sample_time(),
        Some(packet_end)
    );
    assert_eq!(session.pipeline.written_audio_end(), Some(packet_end));
}

#[test]
fn hole_boundaries_follow_the_mid_stream_policy() {
    let (session, _output_factory_handle) = {
        let (mut session, handle) = session_with_audio(SelectedVideo::Selected);
        feed_tone_packets(&mut session, 0..1);
        (session, handle)
    };
    let expected_next = packet_start(1);
    let plan = |gap: Duration| session.plan_audio_segment_start(expected_next + gap, SAMPLE_RATE);

    // До 20 мс включительно — шум меток времени.
    assert_eq!(
        plan(Duration::from_millis(20)),
        AudioSegmentStartPlan::ContinuesSegment
    );
    assert_eq!(
        plan(Duration::from_millis(21)),
        AudioSegmentStartPlan::MidStreamGap(MidStreamGapPlan::Pad {
            expected_next,
            first_sample: expected_next + Duration::from_millis(21),
            silence_frames: 1_008,
            gap: Duration::from_millis(21),
        })
    );
    // Ровно порог передачи часов — ещё тишина; дальше — передача часов (`audio_gap.rs`).
    assert!(matches!(
        plan(Duration::from_millis(500)),
        AudioSegmentStartPlan::MidStreamGap(MidStreamGapPlan::Pad {
            silence_frames: 24_000,
            ..
        })
    ));
    assert!(matches!(
        plan(Duration::from_millis(501)),
        AudioSegmentStartPlan::MidStreamGap(MidStreamGapPlan::AboveSilenceCap { .. })
    ));
}

#[test]
fn hole_above_the_gap_threshold_reaching_decode_keeps_the_old_behaviour() {
    // Packet, отданный прямо в decode (минуя удержание `audio_gap.rs`): прежняя склейка.
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    feed_tone_packets(&mut session, 0..5);

    feed_packet(
        &mut session,
        packet_start(5) + Duration::from_millis(800),
        TONE_PACKET,
    );

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(silent_frames(&pcm), 0);
    assert_eq!(written_frames(&pcm), 6 * PACKET_FRAMES);
    assert_eq!(session.snapshot().last_error, None);
}

#[test]
fn contiguous_and_overlapping_packets_get_no_silence() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    feed_tone_packets(&mut session, 0..5);
    // Повтор/перекрытие (packet раньше курсора): не обрезается и не выравнивается.
    feed_packet(&mut session, packet_start(4), TONE_PACKET);
    feed_tone_packets(&mut session, 5..8);

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(silent_frames(&pcm), 0, "no silence in continuous audio");
    assert_eq!(written_frames(&pcm), 9 * PACKET_FRAMES);
    assert_eq!(
        session.pipeline.expected_next_audio_sample_time(),
        Some(packet_start(8)),
        "the overlapping packet does not move the cursor back"
    );
}

#[test]
fn cursor_survives_a_clock_reanchor_that_forgets_the_written_audio_end() {
    // Seek commit перепривязывает часы, когда в output уже лежит звук отрезка (D3 после
    // перемотки: первый PCM — packet через цель, дыра — следующим packet-ом).
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    feed_tone_packets(&mut session, 0..5);
    session.pipeline.set_media_clock_base(Duration::ZERO);
    assert_eq!(session.pipeline.written_audio_end(), None);
    assert_eq!(
        session.pipeline.expected_next_audio_sample_time(),
        Some(packet_start(5)),
        "the reanchor keeps the segment cursor"
    );
    let hole = Duration::from_millis(321);

    feed_packet(&mut session, packet_start(5) + hole, TONE_PACKET);

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        first_silence(&pcm),
        Some((5 * PACKET_FRAMES, frames_in(hole)))
    );
}

#[test]
fn rate_change_keeps_the_cursor_and_half_speed_stretches_the_hole_silence() {
    let (mut session, _output_factory_handle, tempo_factory_handle) = session_at_rate(0.5);
    assert_eq!(
        session.pipeline.expected_next_audio_sample_time(),
        Some(packet_start(1)),
        "the rate reanchor keeps the segment cursor"
    );
    let hole = Duration::from_millis(300);

    feed_packet(&mut session, packet_start(1) + hole, TONE_PACKET);

    // Тишина идёт в tempo processor раньше звука: на 0,5x он растягивает её вместе со звуком.
    let processed = tempo_factory_handle.processed_inputs();
    assert_eq!(processed.len(), 2, "silence and the packet: {processed:?}");
    assert_eq!(processed[0], vec![0.0; frames_in(hole) * CHANNELS as usize]);
    assert_eq!(processed[1], vec![TONE; PACKET_FRAMES * CHANNELS as usize]);
    assert_eq!(session.snapshot().last_error, None);
}

#[test]
fn new_segment_resets_the_cursor_and_measures_from_the_clock_base() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    feed_tone_packets(&mut session, 0..5);
    let seek_target = Duration::from_secs(3);

    let generation = session.pipeline.begin_seek_generation();
    let _clear_ack = session.pipeline.clear_audio_output_for_seek(generation);
    session.pipeline.reset_clocks_for_seek(seek_target);
    assert_eq!(session.pipeline.expected_next_audio_sample_time(), None);
    let written_before = written_frames(&written_pcm(&output_factory_handle));
    feed_packet(&mut session, seek_target, TONE_PACKET);

    // Первый PCM нового отрезка звучит в цели: тишины нет, хотя старый курсор был на 0,1 с.
    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(silent_frames(&pcm[written_before * CHANNELS as usize..]), 0);
    assert_eq!(
        session.pipeline.expected_next_audio_sample_time(),
        Some(seek_target + packet_start(1))
    );
}

#[test]
fn audio_only_hole_is_padded_too_so_the_position_stays_truthful() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::None);
    feed_tone_packets(&mut session, 0..3);
    let hole = Duration::from_millis(250);

    feed_packet(&mut session, packet_start(3) + hole, TONE_PACKET);

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        first_silence(&pcm),
        Some((3 * PACKET_FRAMES, frames_in(hole)))
    );
}

#[test]
fn live_timeline_hole_is_not_padded() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    session.snapshot.timeline.mode = TimelineMode::Live;
    feed_tone_packets(&mut session, 0..3);

    feed_packet(
        &mut session,
        packet_start(3) + Duration::from_millis(250),
        TONE_PACKET,
    );

    assert_eq!(silent_frames(&written_pcm(&output_factory_handle)), 0);
}

#[test]
fn decode_error_leaves_the_cursor_so_the_next_packet_waits_for_its_time() {
    let (mut session, output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    feed_tone_packets(&mut session, 0..3);

    feed_packet(&mut session, packet_start(3), CORRUPT_PACKET);
    let cursor = session
        .pipeline
        .expected_next_audio_sample_time()
        .expect("the segment has written audio");
    assert!(
        cursor.abs_diff(packet_start(3)) <= Duration::from_micros(1),
        "a packet that was not decoded is not written audio: cursor {cursor:?}"
    );
    feed_packet(&mut session, packet_start(4), TONE_PACKET);

    // Пропавший AAC-кадр (21,33 мс) заменён тишиной той же длины, звук не уехал раньше.
    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        first_silence(&pcm),
        Some((3 * PACKET_FRAMES, PACKET_FRAMES))
    );
}

#[test]
fn codec_relay_without_a_hole_inserts_no_silence() {
    // Смена кодека посреди потока (эстафета 03d): decoder меняется, output и отрезок — нет.
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session =
        PlayerSession::with_audio_factories(Arc::new(ToneDecoderFactory), output_factory);
    let audio_track = fake_audio_track_with_codec(AUDIO_TRACK.get(), "A_AAC");
    install_fake_media(&mut session, vec![audio_track.clone()]);
    session.init_audio_pipeline(std::slice::from_ref(&audio_track));
    let generation = session.pipeline.seek_generation();
    let packet = |index| {
        PendingAudioPacket::new_unbounded(
            AUDIO_TRACK,
            packet_start(index),
            None,
            None,
            generation,
            Bytes::from_static(TONE_PACKET),
        )
    };
    for index in 0..3 {
        session.pipeline.enqueue_pending_audio_packet(packet(index));
    }
    session.process_pending_audio_packets_with_buffer_limit(1_000.0);

    session.handle_demux_track_list_update(DemuxTrackListUpdate::new(
        vec![fake_audio_track_with_codec(AUDIO_TRACK.get(), "A_OPUS")],
        Some(Duration::from_secs(30)),
    ));
    let next_description = session.pipeline.admitted_audio_track_description();
    for index in 3..6 {
        session
            .pipeline
            .enqueue_pending_audio_packet(packet(index).with_track_description(next_description));
    }
    session.process_pending_audio_packets_with_buffer_limit(1_000.0);

    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(
        output_factory_handle.create_count(),
        1,
        "the output survives"
    );
    assert_eq!(written_frames(&pcm), 6 * PACKET_FRAMES);
    assert_eq!(silent_frames(&pcm), 0, "no silence at the codec relay");
}

#[test]
fn leading_silence_that_does_not_fit_the_output_at_quarter_speed_is_held_not_written() {
    // 600 мс при 0,25x — 2,4 с вывода: больше ring-а. Раньше — фатальная частичная запись.
    let (mut session, output_factory_handle, _tempo_factory_handle) = session_at_rate(0.25);
    let generation = session.pipeline.begin_seek_generation();
    let _clear_ack = session.pipeline.clear_audio_output_for_seek(generation);
    session.pipeline.reset_clocks_for_seek(Duration::ZERO);
    let written_before = written_frames(&written_pcm(&output_factory_handle));
    let audio_start = Duration::from_millis(600);
    enqueue_tone_packet(&mut session, audio_start);

    // До decode: звук ждёт своего PTS тем же путём, что и разрыв больше потолка тишины.
    assert_eq!(
        session.segment_start_audio_schedule(),
        SegmentStartAudioSchedule::ScheduledLater {
            clock_base: Duration::ZERO,
            audio_start,
        }
    );
    session.process_pending_audio_packets_with_buffer_limit(1_000.0);
    assert_eq!(session.pipeline.pending_audio_packet_len(), 1, "held");
    assert_eq!(
        written_frames(&written_pcm(&output_factory_handle)),
        written_before
    );

    // Если packet всё же дошёл до decode (live, где удержания нет): тишина не пишется.
    assert!(matches!(
        session.plan_audio_segment_start(audio_start, SAMPLE_RATE),
        AudioSegmentStartPlan::SilenceExceedsOutputRoom { .. }
    ));
    feed_packet(&mut session, audio_start, TONE_PACKET);
    let pcm = written_pcm(&output_factory_handle);
    assert_eq!(silent_frames(&pcm[written_before * CHANNELS as usize..]), 0);
    assert_eq!(session.snapshot().last_error, None);
    assert_ne!(session.playback_state(), PlaybackState::Failed);
}

#[test]
fn quarter_speed_hole_whose_silence_does_not_fit_waits_while_playing() {
    let (mut session, _output_factory_handle, _tempo_factory_handle) = session_at_rate(0.25);
    session.set_playback_state(PlaybackState::Playing);

    // 400 мс × 4 = 1,6 с вывода — влезает в пустой вывод: тишина.
    enqueue_tone_packet(&mut session, packet_start(1) + Duration::from_millis(400));
    assert_eq!(
        session.pending_audio_output_admission(),
        PendingAudioOutputAdmission::Admit
    );
    session.pipeline.clear_pending_audio_packets();

    // 480 мс × 4 = 1,92 с вывода — не влезает: packet ждёт (передача часов разрыва).
    enqueue_tone_packet(&mut session, packet_start(1) + Duration::from_millis(480));
    assert_eq!(
        session.pending_audio_output_admission(),
        PendingAudioOutputAdmission::HoldForAudioGap
    );
    session.process_pending_audio_packets_with_buffer_limit(1_000.0);
    assert_eq!(session.pipeline.pending_audio_packet_len(), 1, "held");
    assert_eq!(session.snapshot().last_error, None);
}

/// Состояние ring-а вывода с ёмкостью: сколько frames лежит и что записано.
#[derive(Default)]
struct RingState {
    /// Frames в ring-е (ещё не проиграно).
    buffered_frames: usize,
    /// Весь принятый PCM.
    written_samples: Vec<f32>,
}

/// Output с ёмкостью ring-а, как у `audio::output`: не влезшее отбрасывается частично.
struct CapacityLimitedOutput {
    ring: Arc<Mutex<RingState>>,
}

/// Часы, стоящие в нуле (тесту важен только уровень ring-а).
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

impl PlayerAudioOutput for CapacityLimitedOutput {
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: crate::AudioOutputWriteIntent,
    ) -> std::result::Result<crate::AudioOutputWriteReport, crate::AudioOutputWriteError> {
        let mut ring = self.ring.lock().expect("ring state lock");
        let frames = samples.len() / CHANNELS as usize;
        let queued = frames.min(RING_CAPACITY_FRAMES - ring.buffered_frames);
        ring.buffered_frames += queued;
        ring.written_samples
            .extend_from_slice(&samples[..queued * CHANNELS as usize]);
        crate::AudioOutputWriteReport::try_new(
            crate::AudioOutputInputFrameCount::new(frames),
            crate::AudioOutputStreamFrameCount::new(frames),
            crate::AudioOutputStreamFrameCount::new(queued),
        )
    }

    fn play(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<audio_core::AudioOutputClockTiming> {
        Ok(StoppedClock.output_timing())
    }

    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        self.ring.lock().expect("ring state lock").buffered_frames = 0;
        Ok(generation)
    }

    fn set_volume(&mut self, _volume: f32) {}

    fn buffer_level_ms(&self) -> f64 {
        let buffered_frames = self.ring.lock().expect("ring state lock").buffered_frames;
        buffered_frames as f64 * 1_000.0 / f64::from(SAMPLE_RATE)
    }

    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        Arc::new(StoppedClock)
    }
}

/// A/V session с output-ом ёмкостью 2 с, в котором уже лежит `packets` AAC-кадров.
fn session_with_filled_ring(packets: u64) -> (PlayerSession, Arc<Mutex<RingState>>) {
    let (mut session, _output_factory_handle) = session_with_audio(SelectedVideo::Selected);
    let ring = Arc::new(Mutex::new(RingState::default()));
    session
        .pipeline
        .install_audio_output_for_tests(Box::new(CapacityLimitedOutput { ring: ring.clone() }));
    session.pipeline.install_audio_clock(Arc::new(StoppedClock));
    feed_tone_packets(&mut session, 0..packets);
    (session, ring)
}

#[test]
fn hole_silence_that_would_overflow_the_ring_is_not_written_without_a_hold() {
    // В ring-е 1,71 с звука (80 кадров), дыра 450 мс: 2,16 с — больше ёмкости 2 с.
    // Раньше тишина писалась и частичная запись роняла воспроизведение.
    let (mut session, ring) = session_with_filled_ring(80);
    session.set_playback_state(PlaybackState::Paused);

    feed_packet(
        &mut session,
        packet_start(80) + Duration::from_millis(450),
        TONE_PACKET,
    );

    assert_eq!(session.snapshot().last_error, None);
    assert_ne!(session.playback_state(), PlaybackState::Failed);
    let ring = ring.lock().expect("ring state lock");
    assert_eq!(silent_frames(&ring.written_samples), 0);
    assert_eq!(written_frames(&ring.written_samples), 81 * PACKET_FRAMES);
}

#[test]
fn hole_silence_that_does_not_fit_waits_for_room_while_playing_then_pads() {
    let (mut session, ring) = session_with_filled_ring(80);
    session.set_playback_state(PlaybackState::Playing);
    let hole = Duration::from_millis(450);
    enqueue_tone_packet(&mut session, packet_start(80) + hole);

    session.process_pending_audio_packets_with_buffer_limit(10_000.0);
    assert_eq!(session.pipeline.pending_audio_packet_len(), 1, "held");
    assert_eq!(
        silent_frames(&ring.lock().expect("ring state lock").written_samples),
        0
    );

    // Карта проиграла почти всё: место для тишины появилось.
    ring.lock().expect("ring state lock").buffered_frames = frames_in(Duration::from_millis(100));
    session.process_pending_audio_packets_with_buffer_limit(10_000.0);

    assert_eq!(session.pipeline.pending_audio_packet_len(), 0);
    let ring = ring.lock().expect("ring state lock");
    assert_eq!(
        first_silence(&ring.written_samples),
        Some((80 * PACKET_FRAMES, frames_in(hole)))
    );
    assert_eq!(session.snapshot().last_error, None);
}
