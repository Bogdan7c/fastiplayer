//! Смена формата decoded PCM посреди потока пересобирает audio output без потери A/V sync.
//!
//! Каждый сценарий проверяет наблюдаемый результат: какой output создан и с каким spec-ом,
//! какие samples дошли до нового output-а, какое media-время показывает presentation clock
//! в момент, когда новый output начинает играть первый настоящий PCM нового формата.

use audio_core::{AudioChannelLayout, AudioPacketTiming};
use bytes::Bytes;
use media_core::PacketPresentationWindow;

use super::test_support::{
    ScriptedAudioDevices, ScriptedAudioOutputFactory, ScriptedAudioOutputFactoryHandle,
    ScriptedAudioOutputHandle,
};
use super::*;
use crate::session::audio_output_recovery::AudioOutputRecoveryOutcome;
use crate::{
    AudioOutputDeviceRequest, AudioOutputSwitchReason, AudioOutputWriteIntent,
    AudioTempoDecodedMedia, AudioTempoFrameCount, AudioTempoOutputProgressMapping,
    AudioTempoPcmFormat, AudioTempoProcessReport, AudioTempoProcessor, AudioTempoProcessorConfig,
    AudioTempoProcessorFactory, AudioTempoProcessorHandle, AudioTempoReportFrameCounts,
    AudioTempoSegment, AudioTempoStretchedOutput, PlaybackRate, PlayerCommandOutcome,
};
use audio_core::{AudioTempoOutputSegmentSpan, AudioTempoOutputSegmentSpans};

/// Audio track тестового media.
const AUDIO_TRACK: u32 = 2;

/// Stereo 48 кГц PCM (2 кадра).
const STEREO_PCM: [f32; 4] = [0.25, -0.25, 0.5, -0.5];

/// 5.1 48 кГц PCM (2 кадра, у каждого канала своё значение).
const SURROUND_PCM: [f32; 12] = [
    0.11, 0.12, 0.13, 0.14, 0.15, 0.16, 0.21, 0.22, 0.23, 0.24, 0.25, 0.26,
];

/// Stereo 44.1 кГц PCM (2 кадра).
const STEREO_44K_PCM: [f32; 4] = [0.31, -0.31, 0.32, -0.32];

/// Payload-ы encoded packet-ов: fake decoder выбирает по ним формат.
const STEREO_PACKET: &[u8] = b"stereo-48k";
const SURROUND_PACKET: &[u8] = b"surround-48k";
const STEREO_44K_PACKET: &[u8] = b"stereo-44k";

/// Сколько PCM старый output уже принял, но ещё не проиграл в момент смены формата.
const UNPLAYED_TAIL: Duration = Duration::from_millis(200);

/// Слышимая позиция clock-а старого output-а в момент смены формата.
const HANDOVER_AUDIBLE: Duration = Duration::from_secs(1);

/// Допуск A/V: округление тишины до кадра — десятки микросекунд, берём 1 мс.
const SYNC_TOLERANCE: Duration = Duration::from_millis(1);

fn stereo_spec() -> AudioOutputSpec {
    AudioOutputSpec::new(48_000, AudioChannelLayout::stereo())
}

fn surround_spec() -> AudioOutputSpec {
    AudioOutputSpec::new(48_000, AudioChannelLayout::surround_5_1())
}

fn stereo_44k_spec() -> AudioOutputSpec {
    AudioOutputSpec::new(44_100, AudioChannelLayout::stereo())
}

/// Decoder любого «кодека», чей формат PCM меняется от packet-а к packet-у.
///
/// Нейтральный контракт `AudioDecoder`: spec описывает именно последний buffer.
struct FormatSwitchingDecoder {
    /// Spec последнего decoded buffer-а.
    current_spec: AudioOutputSpec,
}

impl FormatSwitchingDecoder {
    /// PCM и spec для payload-а packet-а.
    fn block_for(payload: &[u8]) -> anyhow::Result<(Vec<f32>, AudioOutputSpec)> {
        match payload {
            STEREO_PACKET => Ok((STEREO_PCM.to_vec(), stereo_spec())),
            SURROUND_PACKET => Ok((SURROUND_PCM.to_vec(), surround_spec())),
            STEREO_44K_PACKET => Ok((STEREO_44K_PCM.to_vec(), stereo_44k_spec())),
            other => anyhow::bail!("unexpected test payload {other:?}"),
        }
    }
}

impl audio_core::AudioDecoder for FormatSwitchingDecoder {
    fn decode(&mut self, packet: &audio_core::EncodedAudioPacket<'_>) -> anyhow::Result<Vec<f32>> {
        let (samples, spec) = Self::block_for(packet.data())?;
        self.current_spec = spec;
        Ok(samples)
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.current_spec.sample_rate
    }

    fn channels(&self) -> u32 {
        self.current_spec.channels()
    }

    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        Some(self.current_spec.channel_layout)
    }
}

/// Session с format-switching decoder-ом; звук ещё не создан.
fn session_with_switching_decoder() -> (PlayerSession, ScriptedAudioOutputFactoryHandle) {
    let (factory, factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(factory);
    install_switching_decoder(&mut session);
    (session, factory_handle)
}

/// Выбирает audio track и ставит format-switching decoder.
fn install_switching_decoder(session: &mut PlayerSession) {
    let track_id = TrackId::new(AUDIO_TRACK);
    session.pipeline.select_audio_track(track_id);
    session.snapshot.selected_tracks.audio_track = Some(track_id);
    session
        .pipeline
        .install_audio_decoder(Box::new(FormatSwitchingDecoder {
            current_spec: stereo_spec(),
        }));
}

/// Прогоняет один packet через pending queue → decoder → output.
fn feed_packet(session: &mut PlayerSession, payload: &'static [u8], pts: Duration) {
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::with_timing(
            TrackId::new(AUDIO_TRACK),
            pts,
            AudioPacketTiming::unknown(),
            PacketPresentationWindow::Unbounded,
            session.pipeline.seek_generation(),
            Bytes::from_static(payload),
        ));
    session.process_pending_audio_packets_with_buffer_limit(200.0);
}

/// Играющая session со stereo output-ом, у которого 200 мс PCM ещё не проиграно.
fn playing_stereo_session_with_tail() -> (
    PlayerSession,
    ScriptedAudioOutputFactoryHandle,
    ScriptedAudioOutputHandle,
) {
    let (mut session, factory_handle) = session_with_switching_decoder();
    session.begin_playback_without_media_for_test().unwrap();
    feed_packet(&mut session, STEREO_PACKET, Duration::ZERO);
    let first_output = factory_handle.last_output_handle().expect("first output");
    first_output.set_underlying_output_timing(HANDOVER_AUDIBLE, HANDOVER_AUDIBLE + UNPLAYED_TAIL);
    let _ = session.take_events();
    (session, factory_handle, first_output)
}

/// Media-время presentation clock-а, когда новый output дошёл до clock-позиции `played`.
fn media_time_when_output_reaches(
    session: &PlayerSession,
    output: &ScriptedAudioOutputHandle,
    played: Duration,
) -> Duration {
    output.set_underlying_output_timing(played, played);
    session.presentation_clock_position_at(Instant::now())
}

/// Сколько кадров тишины соответствует хвосту на заданной частоте.
fn tail_frames(sample_rate: u32) -> usize {
    (UNPLAYED_TAIL.as_secs_f64() * f64::from(sample_rate)).round() as usize
}

/// Ожидаемое содержимое нового output-а: тишина длиной хвоста, затем PCM нового формата.
fn silence_then(spec: AudioOutputSpec, pcm: &[f32]) -> Vec<f32> {
    let mut expected = vec![0.0; tail_frames(spec.sample_rate) * spec.channels() as usize];
    expected.extend_from_slice(pcm);
    expected
}

fn assert_in_sync(actual: Duration, expected: Duration) {
    assert!(
        actual.abs_diff(expected) <= SYNC_TOLERANCE,
        "audio diverges from video: clock {actual:?}, expected {expected:?}"
    );
}

/// Ни одной ошибки (ни fatal, ни recoverable) в событиях session.
fn assert_no_error_events(events: &[PlayerEvent]) {
    let errors: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                PlayerEvent::RecoverableError(_) | PlayerEvent::FatalError(_)
            )
        })
        .collect();
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn stereo_to_surround_and_back_rebuilds_output_and_keeps_clock_continuous() {
    let (mut session, factory_handle, first_output) = playing_stereo_session_with_tail();
    assert_eq!(
        session.presentation_clock_position_at(Instant::now()),
        HANDOVER_AUDIBLE
    );

    feed_packet(
        &mut session,
        SURROUND_PACKET,
        HANDOVER_AUDIBLE + UNPLAYED_TAIL,
    );

    let surround_output = factory_handle.last_output_handle().expect("5.1 output");
    assert_eq!(
        factory_handle.created_specs(),
        [stereo_spec(), surround_spec()]
    );
    assert_eq!(
        session.pipeline.audio_output_input_spec(),
        Some(surround_spec())
    );
    assert_eq!(
        surround_output.written_samples(),
        silence_then(surround_spec(), &SURROUND_PCM)
    );
    // 5.1 PCM не попал в старый stereo output: там только прежний stereo.
    assert_eq!(first_output.written_samples(), STEREO_PCM);
    assert_eq!(surround_output.play_count(), 1);
    // Позиция не прыгнула в момент стыка и совпадает с PTS первого 5.1 PCM после хвоста.
    assert_eq!(
        session.presentation_clock_position_at(Instant::now()),
        HANDOVER_AUDIBLE
    );
    assert_in_sync(
        media_time_when_output_reaches(&session, &surround_output, UNPLAYED_TAIL),
        HANDOVER_AUDIBLE + UNPLAYED_TAIL,
    );

    // Обратно в stereo: 5.1 output проиграл 500 мс, ещё 200 мс в очереди.
    surround_output.set_underlying_output_timing(
        Duration::from_millis(500),
        Duration::from_millis(500) + UNPLAYED_TAIL,
    );
    let second_handover = HANDOVER_AUDIBLE + Duration::from_millis(500);
    feed_packet(&mut session, STEREO_PACKET, second_handover + UNPLAYED_TAIL);

    let stereo_output = factory_handle.last_output_handle().expect("stereo output");
    assert_eq!(
        factory_handle.created_specs(),
        [stereo_spec(), surround_spec(), stereo_spec()]
    );
    assert_eq!(
        stereo_output.written_samples(),
        silence_then(stereo_spec(), &STEREO_PCM)
    );
    assert_eq!(
        surround_output.written_samples(),
        silence_then(surround_spec(), &SURROUND_PCM)
    );
    assert_in_sync(
        media_time_when_output_reaches(&session, &stereo_output, UNPLAYED_TAIL),
        second_handover + UNPLAYED_TAIL,
    );

    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_no_error_events(&session.take_events());
    // Тот же выбранный device: ни уведомлений, ни смены устройства.
    assert_eq!(
        factory_handle.device_requests(),
        [AudioOutputDeviceRequest::SelectedOrSystemDefault; 3]
    );
}

#[test]
fn previous_output_keeps_receiving_controls_until_its_tail_has_played() {
    let (mut session, _factory_handle, first_output) = playing_stereo_session_with_tail();
    feed_packet(
        &mut session,
        SURROUND_PACKET,
        HANDOVER_AUDIBLE + UNPLAYED_TAIL,
    );

    // Хвост ещё звучит: mute во время стыка глушит и его.
    session.set_volume(0.0).expect("volume");
    assert_eq!(first_output.last_applied_volume(), Some(0.0));

    // Хвост доигран: следующая запись закрывает старый поток, громкость до него не доходит.
    let tail_end = HANDOVER_AUDIBLE + UNPLAYED_TAIL;
    first_output.set_underlying_output_timing(tail_end, tail_end);
    feed_packet(&mut session, SURROUND_PACKET, tail_end + UNPLAYED_TAIL);
    session.set_volume(0.7).expect("volume");
    assert_eq!(first_output.last_applied_volume(), Some(0.0));
}

#[test]
fn sample_rate_change_rebuilds_output_at_new_rate_with_tail_length_silence() {
    let (mut session, factory_handle, _first_output) = playing_stereo_session_with_tail();

    feed_packet(
        &mut session,
        STEREO_44K_PACKET,
        HANDOVER_AUDIBLE + UNPLAYED_TAIL,
    );

    let output_44k = factory_handle
        .last_output_handle()
        .expect("44.1 kHz output");
    assert_eq!(
        factory_handle.created_specs(),
        [stereo_spec(), stereo_44k_spec()]
    );
    // 200 мс на 44.1 кГц = 8820 кадров тишины, затем PCM без изменений.
    assert_eq!(
        output_44k.written_samples(),
        silence_then(stereo_44k_spec(), &STEREO_44K_PCM)
    );
    assert_eq!(output_44k.written_samples().len(), 8_820 * 2 + 4);
    assert_in_sync(
        media_time_when_output_reaches(&session, &output_44k, UNPLAYED_TAIL),
        HANDOVER_AUDIBLE + UNPLAYED_TAIL,
    );
    assert_no_error_events(&session.take_events());
}

#[test]
fn identical_decoded_spec_never_rebuilds_output() {
    let (mut session, factory_handle, first_output) = playing_stereo_session_with_tail();

    feed_packet(&mut session, STEREO_PACKET, Duration::from_millis(50));
    feed_packet(&mut session, STEREO_PACKET, Duration::from_millis(100));

    assert_eq!(factory_handle.create_count(), 1);
    assert_eq!(first_output.written_samples(), [STEREO_PCM; 3].concat());
    assert_eq!(first_output.play_count(), 1);
}

#[test]
fn change_while_paused_waits_for_play_and_resumes_both_streams_in_sync() {
    let (mut session, factory_handle, first_output) = playing_stereo_session_with_tail();
    session.pause().expect("pause");
    assert_eq!(first_output.pause_count(), 1);

    feed_packet(
        &mut session,
        SURROUND_PACKET,
        HANDOVER_AUDIBLE + UNPLAYED_TAIL,
    );

    let surround_output = factory_handle.last_output_handle().expect("5.1 output");
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert_eq!(
        surround_output.play_count(),
        0,
        "paused: new stream must wait"
    );
    assert_eq!(
        surround_output.written_samples(),
        silence_then(surround_spec(), &SURROUND_PCM)
    );
    assert_eq!(
        session.presentation_clock_position_at(Instant::now()),
        HANDOVER_AUDIBLE
    );

    session.begin_playback_without_media_for_test().unwrap();

    assert_eq!(surround_output.play_count(), 1);
    assert_eq!(first_output.play_count(), 2, "the old tail resumes too");
    assert_in_sync(
        media_time_when_output_reaches(&session, &surround_output, UNPLAYED_TAIL),
        HANDOVER_AUDIBLE + UNPLAYED_TAIL,
    );
    assert_no_error_events(&session.take_events());
}

#[test]
fn failed_new_output_disables_audio_but_video_keeps_position() {
    let (mut session, factory_handle, _first_output) = playing_stereo_session_with_tail();
    factory_handle.set_devices(ScriptedAudioDevices {
        selected_connected: false,
        system_default_connected: false,
    });

    feed_packet(
        &mut session,
        SURROUND_PACKET,
        HANDOVER_AUDIBLE + UNPLAYED_TAIL,
    );

    let events = session.take_events();
    let device_errors = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                PlayerEvent::RecoverableError(error)
                    if error.kind == PlayerErrorKind::AudioDeviceUnavailable
            )
        })
        .count();
    assert_eq!(device_errors, 1);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::FatalError(_)))
    );
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert!(!session.pipeline.has_audio_output());
    assert_eq!(session.snapshot.selected_tracks.audio_track, None);
    // Видео продолжает с позиции стыка на monotonic clock-е, а не с нуля.
    assert_in_sync(
        session.presentation_clock_position_at(Instant::now()),
        HANDOVER_AUDIBLE,
    );
}

#[test]
fn format_change_after_default_fallback_stays_on_default_and_spends_no_recovery_budget() {
    let (mut session, factory_handle, first_output) = playing_stereo_session_with_tail();
    let started_at = Instant::now();
    factory_handle.set_devices(ScriptedAudioDevices {
        selected_connected: true,
        system_default_connected: true,
    });
    // Попытка 1 из 3: поток сломался → default.
    first_output.fail_stream();
    assert_eq!(
        session.recover_failed_audio_output_if_needed(started_at),
        AudioOutputRecoveryOutcome::SwitchedToSystemDefault
    );
    let _ = session.take_events();

    feed_packet(
        &mut session,
        SURROUND_PACKET,
        HANDOVER_AUDIBLE + UNPLAYED_TAIL,
    );

    // Смена формата не вернула звук на выбранное устройство и не уведомила о переходе.
    assert_eq!(
        factory_handle.device_requests(),
        [
            AudioOutputDeviceRequest::SelectedOrSystemDefault,
            AudioOutputDeviceRequest::SystemDefault,
            AudioOutputDeviceRequest::SystemDefault,
        ]
    );
    let events = session.take_events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::AudioOutputSwitchedToSystemDefault(_)))
    );
    assert_no_error_events(&events);

    // Бюджет не потрачен и не сброшен: остаются ровно попытки 2 и 3, четвёртой нет.
    for attempt in 2..=3_u64 {
        factory_handle
            .last_output_handle()
            .expect("active output")
            .fail_stream();
        assert_eq!(
            session.recover_failed_audio_output_if_needed(
                started_at + Duration::from_secs(2 * attempt)
            ),
            AudioOutputRecoveryOutcome::SwitchedToSystemDefault
        );
    }
    factory_handle
        .last_output_handle()
        .expect("active output")
        .fail_stream();
    assert_eq!(
        session.recover_failed_audio_output_if_needed(started_at + Duration::from_secs(10)),
        AudioOutputRecoveryOutcome::AudioDisabled
    );
    assert_eq!(
        switch_reasons(&session.take_events()),
        vec![
            AudioOutputSwitchReason::ActiveDeviceStopped,
            AudioOutputSwitchReason::ActiveDeviceStopped,
        ]
    );
}

fn switch_reasons(events: &[PlayerEvent]) -> Vec<AudioOutputSwitchReason> {
    events
        .iter()
        .filter_map(|event| match event {
            PlayerEvent::AudioOutputSwitchedToSystemDefault(reason) => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn change_at_1_5x_flushes_old_dsp_tail_and_restarts_tempo_in_new_format() {
    let (output_factory, factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let tempo_formats = Arc::new(Mutex::new(Vec::new()));
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(Arc::new(IdentityTempoFactory {
            created_formats: tempo_formats.clone(),
        }));
    install_switching_decoder(&mut session);
    session.begin_playback_without_media_for_test().unwrap();
    feed_packet(&mut session, STEREO_PACKET, Duration::ZERO);
    let first_output = factory_handle.last_output_handle().expect("first output");
    // До смены скорости весь принятый PCM уже прозвучал (хвост 0).
    first_output.set_underlying_output_timing(HANDOVER_AUDIBLE, HANDOVER_AUDIBLE);
    let fast = PlaybackRate::new(1.5).expect("valid rate");
    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(fast))
            .expect("rate command"),
        PlayerCommandOutcome::Applied
    );
    feed_packet(&mut session, STEREO_PACKET, Duration::from_millis(50));
    // На 1.5x старый output проиграл ещё 500 мс (= 750 мс media), 200 мс в очереди.
    let audible_at_change = HANDOVER_AUDIBLE + Duration::from_millis(500);
    first_output.set_underlying_output_timing(audible_at_change, audible_at_change + UNPLAYED_TAIL);
    let handover_media = HANDOVER_AUDIBLE + Duration::from_millis(750);
    assert_in_sync(
        session.presentation_clock_position_at(Instant::now()),
        handover_media,
    );

    feed_packet(
        &mut session,
        SURROUND_PACKET,
        handover_media + UNPLAYED_TAIL,
    );

    let surround_output = factory_handle.last_output_handle().expect("5.1 output");
    // DSP-хвост stereo processor-а дописан в старый output, а не потерян и не в новый.
    assert_eq!(
        first_output.written_samples().last_chunk::<2>(),
        Some(&[FINISH_MARKER; 2])
    );
    assert_eq!(
        *tempo_formats.lock().expect("formats lock"),
        [
            AudioTempoPcmFormat::from_audio_output_spec(stereo_spec()).expect("stereo format"),
            AudioTempoPcmFormat::from_audio_output_spec(surround_spec()).expect("5.1 format"),
        ]
    );
    assert_eq!(
        surround_output.written_samples(),
        silence_then(surround_spec(), &SURROUND_PCM)
    );
    assert_eq!(
        surround_output.written_intents(),
        [
            AudioOutputWriteIntent::DirectDecodedPcm,
            AudioOutputWriteIntent::TempoProcessed,
        ]
    );
    // Тишина 200 мс на 1.5x = 300 мс media: новый PCM звучит ровно после старого хвоста.
    assert_in_sync(
        media_time_when_output_reaches(&session, &surround_output, UNPLAYED_TAIL),
        handover_media + Duration::from_millis(300),
    );
    assert_eq!(session.snapshot.playback_rate, fast);
    assert_no_error_events(&session.take_events());
}

/// Скорость сменилась, когда в старом output-е ещё 200 мс PCM, произведённого на 1x,
/// и сразу за этим пришёл PCM нового формата. Хвост доигрывает на 1x, поэтому новый
/// формат обязан зазвучать в `стык + 200 мс`, а не в `стык + 200 мс × 1.5`
/// (якорь с одной текущей скоростью терял rate-сегменты хвоста: дрейф 100 мс).
#[test]
fn rate_change_inside_unplayed_tail_keeps_sync_across_format_change() {
    let (output_factory, factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(Arc::new(IdentityTempoFactory {
            created_formats: Arc::new(Mutex::new(Vec::new())),
        }));
    install_switching_decoder(&mut session);
    session.begin_playback_without_media_for_test().unwrap();
    feed_packet(&mut session, STEREO_PACKET, Duration::ZERO);
    let first_output = factory_handle.last_output_handle().expect("first output");
    first_output.set_underlying_output_timing(HANDOVER_AUDIBLE, HANDOVER_AUDIBLE + UNPLAYED_TAIL);
    let handover_media = session.presentation_clock_position_at(Instant::now());
    let fast = PlaybackRate::new(1.5).expect("valid rate");
    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(fast))
            .expect("rate command"),
        PlayerCommandOutcome::Applied
    );

    feed_packet(
        &mut session,
        SURROUND_PACKET,
        handover_media + UNPLAYED_TAIL,
    );

    let surround_output = factory_handle.last_output_handle().expect("5.1 output");
    assert_ne!(
        surround_output.written_samples(),
        first_output.written_samples()
    );
    // Хвост (200 мс на 1x) = 200 мс media; дальше уже 1.5x.
    assert_in_sync(
        media_time_when_output_reaches(&session, &surround_output, UNPLAYED_TAIL),
        handover_media + UNPLAYED_TAIL,
    );
    assert_in_sync(
        media_time_when_output_reaches(
            &session,
            &surround_output,
            UNPLAYED_TAIL + Duration::from_millis(100),
        ),
        handover_media + UNPLAYED_TAIL + Duration::from_millis(150),
    );
    assert_eq!(session.snapshot.playback_rate, fast);
    assert_no_error_events(&session.take_events());
}

/// Значение, которым fake processor помечает свой DSP-хвост при `finish`.
const FINISH_MARKER: f32 = 7.0;

/// Tempo factory, чей processor отдаёт вход без изменений (удобно сверять samples).
struct IdentityTempoFactory {
    /// PCM-форматы созданных processor-ов в порядке создания.
    created_formats: Arc<Mutex<Vec<AudioTempoPcmFormat>>>,
}

impl AudioTempoProcessorFactory for IdentityTempoFactory {
    fn create_processor(
        &self,
        config: AudioTempoProcessorConfig,
    ) -> anyhow::Result<AudioTempoProcessorHandle> {
        self.created_formats
            .lock()
            .expect("formats lock")
            .push(config.pcm_format());
        Ok(Box::new(IdentityTempoProcessor {
            pcm_format: config.pcm_format(),
            segment: config.initial_segment(),
        }))
    }
}

/// Processor без растяжения: проверяется маршрут PCM, а не DSP.
struct IdentityTempoProcessor {
    pcm_format: AudioTempoPcmFormat,
    segment: AudioTempoSegment,
}

impl IdentityTempoProcessor {
    /// Report, согласованный с числом выданных кадров.
    fn report(
        &self,
        consumed: AudioTempoFrameCount,
        produced: AudioTempoFrameCount,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        let produced_segments = if produced == AudioTempoFrameCount::ZERO {
            AudioTempoOutputSegmentSpans::Empty
        } else {
            AudioTempoOutputSegmentSpans::One(AudioTempoOutputSegmentSpan::new(
                self.pcm_format,
                self.segment,
                produced,
            ))
        };
        AudioTempoProcessReport::from_frame_counts(
            self.pcm_format,
            self.segment,
            AudioTempoReportFrameCounts {
                consumed_decoded_media: consumed,
                produced_stretched_output: produced,
                pending_processor_output: AudioTempoFrameCount::ZERO,
                input_latency: AudioTempoFrameCount::ZERO,
                output_latency: AudioTempoFrameCount::ZERO,
            },
            AudioTempoOutputProgressMapping::new(
                produced_segments,
                AudioTempoOutputSegmentSpans::Empty,
            ),
        )
    }
}

impl AudioTempoProcessor for IdentityTempoProcessor {
    fn pcm_format(&self) -> AudioTempoPcmFormat {
        self.pcm_format
    }

    fn prime_decoded_history(
        &mut self,
        _decoded_history: AudioTempoDecodedMedia<'_>,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        self.report(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::ZERO)
    }

    fn set_segment(
        &mut self,
        segment: AudioTempoSegment,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        self.segment = segment;
        self.report(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::ZERO)
    }

    fn process_decoded_media_into<'output>(
        &mut self,
        decoded_media: AudioTempoDecodedMedia<'_>,
        output_buffer: &'output mut Vec<f32>,
    ) -> anyhow::Result<AudioTempoStretchedOutput<'output>> {
        anyhow::ensure!(
            decoded_media.pcm_format() == self.pcm_format,
            "processor received PCM of another format"
        );
        output_buffer.clear();
        output_buffer.extend_from_slice(decoded_media.interleaved_samples());
        let frames = decoded_media.frame_count();
        let report = self.report(frames, frames)?;
        AudioTempoStretchedOutput::new(output_buffer.as_slice(), report, self.pcm_format)
    }

    fn finish_stream_into<'output>(
        &mut self,
        output_buffer: &'output mut Vec<f32>,
    ) -> anyhow::Result<AudioTempoStretchedOutput<'output>> {
        let channels = self.pcm_format.channel_count().get() as usize;
        output_buffer.clear();
        output_buffer.resize(channels, FINISH_MARKER);
        let report = self.report(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::new(1))?;
        AudioTempoStretchedOutput::new(output_buffer.as_slice(), report, self.pcm_format)
    }

    fn reset(&mut self) -> anyhow::Result<AudioTempoProcessReport> {
        self.report(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::ZERO)
    }
}
