use std::time::{Duration, Instant};

use media_core::{TrackInfo, TrackKind};
use tracing::{info, warn};

use crate::pipeline::{AudioSeekRuntimeState, DecodedAudioPacket};
use crate::seek_state::{PlaybackResumeIntent, SeekCommitState};
use crate::{
    AudioOutputSpec, PlaybackState, PlayerError, PlayerErrorKind, PlayerResult,
    SeekProgressBlocker, TrackId,
};

use super::{PlayerSession, tick::PlayerTickConfig};

/// Чистый план выбора audio track-а без decoder/output side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AudioDecoderInitSpec {
    /// Track ID выбранного audio трека.
    pub(super) track_id: TrackId,

    /// Container codec id, который audio crate мапит на Symphonia codec id.
    pub(super) codec_id: String,

    /// Codec private / extra data из container metadata.
    pub(super) codec_private: Option<Vec<u8>>,

    /// Sample rate, который demuxer уже сообщил до первого decoded packet-а.
    pub(super) initial_sample_rate: Option<u32>,

    /// Количество каналов, которое demuxer уже сообщил до первого decoded packet-а.
    pub(super) initial_channels: Option<u32>,
}

/// Минимальный защитный preroll, ниже которого seek resume не должен считать audio готовым.
const MIN_SEEK_AUDIO_PREROLL_MS: f64 = 1.0;

/// Минимальный audio preroll для autoplay, защищающий старт от пустого output buffer-а.
const MIN_AUTOPLAY_AUDIO_PREROLL_MS: f64 = 1.0;

/// Typed результат проверки audio readiness для autoplay preroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioAutoplayReadiness {
    /// Audio track не выбран: video-only или отключённый audio path не блокируют autoplay.
    NoSelectedAudio,

    /// Audio track выбран, но decoder ещё не установлен.
    WaitingForDecoder,

    /// Decoder установлен, но output ещё не создан из decoded audio spec.
    WaitingForOutput,

    /// Output готов, но buffer ещё не набрал целевой preroll.
    WaitingForPreroll,

    /// Decoder/output готовы, а buffer достиг целевого preroll.
    Ready,
}

impl AudioAutoplayReadiness {
    /// Возвращает `true` только для состояний, которые не блокируют autoplay.
    pub(super) const fn is_ready(self) -> bool {
        matches!(self, Self::NoSelectedAudio | Self::Ready)
    }
}

/// Typed результат проверки audio gate-а для seek commit-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SeekAudioGateStatus {
    /// Audio gate готов и не блокирует commit.
    Ready,

    /// Output ещё не подтвердил очистку buffer-а текущего seek generation.
    WaitingForClear,

    /// Для выбранного audio track-а ещё нет установленного decoder-а.
    WaitingForDecoder,

    /// Decoder установлен, но output ещё не создан по decoded AudioSpec.
    WaitingForOutput,

    /// Output готов, но buffer ещё не содержит минимальный post-seek preroll.
    WaitingForPreroll,
}

impl SeekAudioGateStatus {
    /// Возвращает `true`, только если status не блокирует seek commit.
    pub(super) const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Мапит audio-gate status в diagnostics blocker без потери причины.
    pub(super) const fn blocker(self) -> Option<SeekProgressBlocker> {
        match self {
            Self::Ready => None,
            Self::WaitingForClear => Some(SeekProgressBlocker::WaitingForAudioClear),
            Self::WaitingForDecoder => Some(SeekProgressBlocker::WaitingForAudioDecoder),
            Self::WaitingForOutput => Some(SeekProgressBlocker::WaitingForAudioOutput),
            Self::WaitingForPreroll => Some(SeekProgressBlocker::WaitingForAudioPreroll),
        }
    }

    /// Возвращает `true` только для blockers, которые можно отпустить soft fallback-ом.
    pub(super) const fn can_soft_fallback(self) -> bool {
        matches!(
            self,
            Self::WaitingForDecoder | Self::WaitingForOutput | Self::WaitingForPreroll
        )
    }
}

impl PlayerSession {
    /// Обрабатывает audio packet: decode -> write to AudioOutput.
    pub fn process_audio_packet(
        &mut self,
        track_id: TrackId,
        packet_pts: Duration,
        _packet_dts: Option<Duration>,
        _packet_duration: Option<Duration>,
        generation: u64,
        encoded_audio_bytes: &[u8],
    ) {
        self.process_audio_packet_with_timing(
            track_id,
            packet_pts,
            audio_core::AudioPacketTiming::unknown(),
            generation,
            encoded_audio_bytes,
        );
    }

    /// Обрабатывает audio packet с raw container timing для decoder boundary.
    pub(crate) fn process_audio_packet_with_timing(
        &mut self,
        track_id: TrackId,
        packet_pts: Duration,
        packet_timing: audio_core::AudioPacketTiming,
        generation: u64,
        encoded_audio_bytes: &[u8],
    ) {
        if self.pipeline.selected_audio_track_id() != Some(track_id) {
            return;
        }

        if !self.pipeline.packet_generation_is_current(generation) {
            return;
        }

        match self.ensure_audio_decoder_for_packet(track_id) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                self.record_recoverable_error(error);
                return;
            }
        }

        let encoded_audio_packet =
            audio_core::EncodedAudioPacket::new(track_id.get(), packet_timing, encoded_audio_bytes);

        if let Some(decode_result) = self.pipeline.decode_audio_packet(&encoded_audio_packet) {
            match decode_result {
                Ok(DecodedAudioPacket::Pcm {
                    samples: decoded_samples,
                    output_spec,
                }) => {
                    if let Err(error) = self.ensure_audio_output_for_decoded_spec(output_spec) {
                        warn!(error = %error, "Не удалось подготовить audio output");
                        if error.kind == PlayerErrorKind::AudioDeviceUnavailable {
                            self.disable_selected_audio_path();
                            self.record_recoverable_error(error);
                        } else {
                            // Format/layout mismatch означает, что уже активный
                            // audio path нельзя продолжить корректно. Video-only
                            // fallback здесь скрыл бы потерю звука.
                            self.mark_fatal_error(error);
                        }
                        return;
                    }

                    let samples = trim_decoded_audio_to_clock_base(
                        &decoded_samples,
                        packet_pts,
                        self.pipeline.media_clock_base(),
                        output_spec.sample_rate,
                        output_spec.channels(),
                    );
                    if samples.is_empty() {
                        return;
                    }

                    if let Err(error) =
                        self.write_decoded_audio_samples_at_current_rate(samples, output_spec)
                    {
                        warn!(error = %error, "Ошибка audio tempo/output path");
                        // После успешного rate commit PCM уже мог попасть в output;
                        // rollback невозможен, поэтому запрещён video-only fallback.
                        self.mark_fatal_error(error);
                    }
                }
                Ok(DecodedAudioPacket::Empty) => {}
                Err(error) => {
                    warn!(error = %error, "Ошибка декодирования audio packet");
                    self.set_runtime_error(format!("Audio decode error: {error}"));
                }
            }
        }
    }

    /// Создаёт deferred audio decoder при первом packet-е выбранного track-а.
    pub(super) fn ensure_audio_decoder_for_packet(
        &mut self,
        track_id: TrackId,
    ) -> PlayerResult<bool> {
        if self.pipeline.has_audio_decoder() {
            return Ok(true);
        }

        let Some(decoder_config) = self.pipeline.take_deferred_audio_decoder_config(track_id)
        else {
            return Ok(false);
        };

        let codec_id = decoder_config.codec_id().to_string();
        match self.audio_decoder_factory.create_decoder(decoder_config) {
            Ok(decoder) => {
                self.pipeline.install_audio_decoder(decoder);
                info!(
                    track_id = %track_id,
                    codec_id = %codec_id,
                    "Deferred audio decoder создан по первому packet-у"
                );
                Ok(true)
            }
            Err(error) => {
                let player_error = player_error_from_audio_decoder_factory_error(error);
                warn!(
                    error = %player_error,
                    codec_id = %codec_id,
                    "Не удалось создать deferred audio decoder"
                );
                self.disable_selected_audio_path();
                Err(player_error)
            }
        }
    }

    /// Создаёт audio output только после того, как decoder сообщил реальный decoded spec.
    pub(super) fn ensure_audio_output_for_decoded_spec(
        &mut self,
        output_spec: AudioOutputSpec,
    ) -> PlayerResult<()> {
        let sample_rate = output_spec.sample_rate;
        let channels = output_spec.channels();
        if self.pipeline.has_audio_output() {
            if let Some(active_spec) = self.pipeline.audio_output_input_spec() {
                if active_spec != output_spec {
                    return Err(PlayerError::new(
                        PlayerErrorKind::RuntimeError,
                        format!(
                            "Decoded audio format changed while output is active: active={active_spec:?}, decoded={output_spec:?}"
                        ),
                    ));
                }
            }
            return Ok(());
        }

        if sample_rate == 0 || channels == 0 {
            return Err(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!(
                    "Audio decoder produced samples without complete AudioSpec: sample_rate={sample_rate}, channels={channels}"
                ),
            ));
        }

        let mut output = self
            .audio_output_factory
            .create_output(output_spec)
            .map_err(|error| {
                PlayerError::new(
                    PlayerErrorKind::AudioDeviceUnavailable,
                    format!("Audio output init failed: {error}"),
                )
            })?;

        output.set_volume(self.snapshot.volume);
        self.pipeline.install_audio_output(output, output_spec);

        if let Some(clock) = self.pipeline.audio_output_clock() {
            self.pipeline.install_audio_clock(clock);
        }
        self.pipeline.reanchor_audio_clock_media_mapping(
            self.pipeline.media_clock_base(),
            self.snapshot.playback_rate,
        );

        if self.playback_state() == PlaybackState::Playing {
            if let Some(Err(error)) = self.pipeline.play_audio_output() {
                return Err(PlayerError::new(
                    PlayerErrorKind::AudioDeviceUnavailable,
                    format!("Audio play after lazy output init failed: {error}"),
                ));
            }

            let observed_at = Instant::now();
            let audio_now = self.audio_clock_now();
            self.pipeline
                .reset_audio_clock_sample(audio_now, observed_at);
        }

        info!(
            sample_rate,
            channels,
            channel_layout = %output_spec.channel_layout,
            "Audio output создан после первого decoded AudioSpec"
        );

        Ok(())
    }

    /// Отключает audio path после unrecoverable lazy-init ошибки, не трогая video state.
    pub(super) fn disable_selected_audio_path(&mut self) {
        self.pipeline.clear_audio_decoder();
        self.pipeline.clear_audio_output();
        self.pipeline.clear_selected_audio_track();
        self.pipeline.clear_pending_audio_packets();
        self.snapshot.selected_tracks.audio_track = None;
    }

    /// Process pending audio packets с throttle по buffer level.
    pub fn process_pending_audio_packets(&mut self) {
        self.process_pending_audio_packets_with_buffer_limit(
            PlayerTickConfig::default().audio_buffer_high_water_mark_ms,
        );
    }

    /// Диагностирует слышимые пропуски звука при активном playback.
    ///
    /// Проверка уровня буфера в конце tick-а слепа к голоданию, которое тот же
    /// tick уже залатал, поэтому основной сигнал — дельта CPAL underrun
    /// callbacks: она растёт при каждом реальном device-side разрыве. Лог несёт
    /// глубины очередей и паузу между tick-ами, чтобы разграничить
    /// video-starved demux, стопор worker-треда и деградацию audio путей.
    pub(super) fn diagnose_audio_output_starvation(&mut self, now: Instant) {
        const STARVATION_LEVEL_MS: f64 = 1.0;
        const WARN_INTERVAL: Duration = Duration::from_secs(2);

        let previous_tick_at = self.last_tick_observed_at.replace(now);

        if self.snapshot.playback_state != PlaybackState::Playing
            || !self.pipeline.has_audio_clock()
            || self.eof_drain_needs_progress()
        {
            self.last_seen_audio_underrun_callbacks =
                self.pipeline.audio_clock_underrun_callbacks();
            return;
        }

        let underrun_callbacks = self.pipeline.audio_clock_underrun_callbacks();
        let new_underruns =
            underrun_callbacks.saturating_sub(self.last_seen_audio_underrun_callbacks);
        self.last_seen_audio_underrun_callbacks = underrun_callbacks;

        let buffer_level_ms = self.audio_buffer_level_ms().unwrap_or(0.0);
        let buffer_starved = buffer_level_ms.is_finite() && buffer_level_ms <= STARVATION_LEVEL_MS;

        if new_underruns == 0 && !buffer_starved {
            return;
        }

        let warn_is_due = self
            .last_audio_starvation_warn_at
            .is_none_or(|last| now.saturating_duration_since(last) >= WARN_INTERVAL);
        if !warn_is_due {
            return;
        }
        self.last_audio_starvation_warn_at = Some(now);

        let tick_gap_ms = previous_tick_at
            .map(|at| now.saturating_duration_since(at).as_secs_f64() * 1000.0)
            .unwrap_or(0.0);

        warn!(
            new_underrun_callbacks = new_underruns,
            buffer_level_ms,
            tick_gap_ms,
            pending_audio_packets = self.pipeline.pending_audio_packet_len(),
            pending_video_packets = self.pipeline.pending_video_packet_len(),
            video_present_queue = self.pipeline.video_present_queue_len(),
            playback_rate = %self.snapshot.playback_rate,
            "Слышимый пропуск звука: CPAL underrun или осушенный buffer при Playing"
        );
    }

    /// Обрабатывает pending audio packets до достижения high-water mark audio buffer.
    pub(crate) fn process_pending_audio_packets_with_buffer_limit(
        &mut self,
        high_water_mark_ms: f64,
    ) {
        let high_water_mark_ms = sanitize_audio_high_water_mark(high_water_mark_ms);

        if self.audio_buffer_level_ms().unwrap_or(0.0) > high_water_mark_ms {
            return;
        }

        while let Some(packet) = self.pipeline.pop_pending_audio_packet_front() {
            if self.audio_buffer_level_ms().unwrap_or(0.0) > high_water_mark_ms {
                self.pipeline.push_pending_audio_packet_front(packet);
                break;
            }

            self.process_audio_packet_with_timing(
                packet.track_id,
                packet.pts,
                packet.timing,
                packet.generation,
                &packet.encoded_bytes,
            );
        }
    }

    /// Возвращает audio clock time для отображения в UI.
    #[must_use]
    pub fn audio_clock_secs(&self) -> Option<f64> {
        self.pipeline
            .audio_output_clock()
            .map(|clock| clock.now().as_secs_f64())
    }

    /// Возвращает уровень audio buffer в миллисекундах.
    #[must_use]
    pub fn audio_buffer_level_ms(&self) -> Option<f64> {
        self.pipeline.audio_output_buffer_level_ms()
    }

    /// Возвращает текущее время audio clock.
    #[must_use]
    pub fn audio_clock_now(&self) -> Duration {
        self.pipeline.audio_clock_now()
    }

    /// Возвращает typed status audio gate-а без схлопывания разных причин в `bool`.
    pub(super) fn seek_audio_gate_status(
        &self,
        seek_commit: SeekCommitState,
        resume_audio_min_buffer_ms: f64,
    ) -> SeekAudioGateStatus {
        classify_seek_audio_gate(
            seek_commit,
            self.pipeline.audio_seek_runtime_state(),
            self.pipeline.audio_buffer_clear_generation(),
            self.audio_buffer_level_ms(),
            resume_audio_min_buffer_ms,
        )
    }

    /// Возвращает typed audio readiness для autoplay без раскрытия pipeline storage.
    pub(super) fn autoplay_audio_readiness(
        &self,
        audio_preroll_target_ms: f64,
    ) -> AudioAutoplayReadiness {
        classify_autoplay_audio_readiness(
            self.pipeline.audio_seek_runtime_state(),
            self.audio_buffer_level_ms(),
            audio_preroll_target_ms,
        )
    }

    /// Выбирает audio track и откладывает decoder/output init до первого packet-а.
    pub(super) fn init_audio_pipeline(&mut self, tracks: &[TrackInfo]) {
        let init_spec = match audio_decoder_init_spec_from_tracks(tracks) {
            Ok(Some(init_spec)) => init_spec,
            Ok(None) => {
                info!("Audio track не найден — playback без звука");
                return;
            }
            Err(error) => {
                warn!(error = %error, "Audio track rejected during lazy decoder planning");
                self.record_recoverable_error(error);
                return;
            }
        };

        info!(
            track_id = %init_spec.track_id,
            codec_id = %init_spec.codec_id,
            sample_rate = ?init_spec.initial_sample_rate,
            channels = ?init_spec.initial_channels,
            "Audio track выбран; decoder/output будут созданы лениво"
        );

        let decoder_config = audio_core::AudioDecoderConfig::from_track_metadata(
            init_spec.track_id.get(),
            init_spec.codec_id.clone(),
            init_spec.initial_sample_rate,
            init_spec.initial_channels,
        )
        .with_codec_private(init_spec.codec_private.clone());

        self.pipeline
            .install_deferred_audio_decoder_config(decoder_config);

        self.pipeline.select_audio_track(init_spec.track_id);
        self.snapshot.selected_tracks.audio_track = Some(init_spec.track_id);

        info!(
            track_id = %init_spec.track_id,
            "Audio pipeline подготовлен к lazy init"
        );
    }
}

/// Нормализует high-water mark, чтобы внешний некорректный config не ломал audio throttle.
pub(super) fn sanitize_audio_high_water_mark(high_water_mark_ms: f64) -> f64 {
    if high_water_mark_ms.is_finite() && high_water_mark_ms > 0.0 {
        high_water_mark_ms
    } else {
        PlayerTickConfig::default().audio_buffer_high_water_mark_ms
    }
}

/// Чистая политика audio gate-а для autoplay preroll.
pub(super) fn classify_autoplay_audio_readiness(
    audio_runtime_state: AudioSeekRuntimeState,
    audio_buffer_level_ms: Option<f64>,
    audio_preroll_target_ms: f64,
) -> AudioAutoplayReadiness {
    match audio_runtime_state {
        AudioSeekRuntimeState::NoSelectedAudio => AudioAutoplayReadiness::NoSelectedAudio,
        AudioSeekRuntimeState::WaitingForDecoder => AudioAutoplayReadiness::WaitingForDecoder,
        AudioSeekRuntimeState::WaitingForOutput => AudioAutoplayReadiness::WaitingForOutput,
        AudioSeekRuntimeState::Ready => {
            if autoplay_audio_preroll_ready(audio_buffer_level_ms, audio_preroll_target_ms) {
                AudioAutoplayReadiness::Ready
            } else {
                AudioAutoplayReadiness::WaitingForPreroll
            }
        }
    }
}

/// Проверяет, набрал ли output buffer минимальный audio preroll для autoplay.
fn autoplay_audio_preroll_ready(
    audio_buffer_level_ms: Option<f64>,
    audio_preroll_target_ms: f64,
) -> bool {
    let required_preroll_ms = audio_preroll_target_ms.max(MIN_AUTOPLAY_AUDIO_PREROLL_MS);

    audio_buffer_level_ms
        .is_some_and(|level_ms| level_ms.is_finite() && level_ms >= required_preroll_ms)
}

/// Чистая политика audio gate-а для seek commit-а.
pub(super) fn classify_seek_audio_gate(
    seek_commit: SeekCommitState,
    audio_runtime_state: AudioSeekRuntimeState,
    audio_buffer_clear_generation: u64,
    audio_buffer_level_ms: Option<f64>,
    resume_audio_min_buffer_ms: f64,
) -> SeekAudioGateStatus {
    if audio_runtime_state == AudioSeekRuntimeState::NoSelectedAudio {
        return SeekAudioGateStatus::Ready;
    }

    if audio_buffer_clear_generation < seek_commit.generation {
        return SeekAudioGateStatus::WaitingForClear;
    }

    if seek_commit.resume_intent == PlaybackResumeIntent::Pause {
        return SeekAudioGateStatus::Ready;
    }

    match audio_runtime_state {
        AudioSeekRuntimeState::NoSelectedAudio => SeekAudioGateStatus::Ready,
        AudioSeekRuntimeState::WaitingForDecoder => SeekAudioGateStatus::WaitingForDecoder,
        AudioSeekRuntimeState::WaitingForOutput => SeekAudioGateStatus::WaitingForOutput,
        AudioSeekRuntimeState::Ready => {
            if seek_audio_preroll_ready(audio_buffer_level_ms, resume_audio_min_buffer_ms) {
                SeekAudioGateStatus::Ready
            } else {
                SeekAudioGateStatus::WaitingForPreroll
            }
        }
    }
}

/// Проверяет минимальный audio buffer после seek, не считая absent output успехом.
fn seek_audio_preroll_ready(
    audio_buffer_level_ms: Option<f64>,
    resume_audio_min_buffer_ms: f64,
) -> bool {
    let Some(audio_buffer_level_ms) = audio_buffer_level_ms else {
        return false;
    };

    audio_buffer_level_ms.is_finite()
        && audio_buffer_level_ms >= normalized_seek_audio_preroll_ms(resume_audio_min_buffer_ms)
}

/// Нормализует внешний config seek-preroll-а к безопасному положительному минимуму.
fn normalized_seek_audio_preroll_ms(resume_audio_min_buffer_ms: f64) -> f64 {
    if resume_audio_min_buffer_ms.is_finite() && resume_audio_min_buffer_ms > 0.0 {
        return resume_audio_min_buffer_ms.max(MIN_SEEK_AUDIO_PREROLL_MS);
    }

    MIN_SEEK_AUDIO_PREROLL_MS
}

/// Находит первый audio track, даже если probe ещё не сообщил полный decoded spec.
fn first_audio_track(tracks: &[TrackInfo]) -> Option<&TrackInfo> {
    tracks.iter().find(|track| track.kind == TrackKind::Audio)
}

/// Создаёт чистый lazy-init plan для audio decoder-а без открытия CPAL device.
fn audio_decoder_init_spec_from_track(track: &TrackInfo) -> PlayerResult<AudioDecoderInitSpec> {
    if track.sample_rate == Some(0) {
        return Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!(
                "Audio track `{}` содержит нулевой sample_rate для decoder init",
                track.id
            ),
        ));
    }

    if track.channels == Some(0) {
        return Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!(
                "Audio track `{}` содержит нулевой channel count для decoder init",
                track.id
            ),
        ));
    }

    Ok(AudioDecoderInitSpec {
        track_id: track.id,
        codec_id: track.codec_id.clone(),
        codec_private: track.codec_private.as_ref().map(|bytes| bytes.to_vec()),
        initial_sample_rate: track.sample_rate,
        initial_channels: track.channels,
    })
}

/// Выбирает audio decoder lazy-init plan по track metadata, не создавая runtime ресурсы.
pub(super) fn audio_decoder_init_spec_from_tracks(
    tracks: &[TrackInfo],
) -> PlayerResult<Option<AudioDecoderInitSpec>> {
    let Some(track) = first_audio_track(tracks) else {
        return Ok(None);
    };

    audio_decoder_init_spec_from_track(track).map(Some)
}

/// Сохраняет typed unsupported-codec ошибку factory, не меняя остальные init errors.
fn player_error_from_audio_decoder_factory_error(error: anyhow::Error) -> PlayerError {
    if matches!(
        error.downcast_ref::<audio_core::AudioDecoderError>(),
        Some(audio_core::AudioDecoderError::UnsupportedCodec { .. })
    ) {
        return PlayerError::new(
            PlayerErrorKind::UnsupportedAudioCodec,
            format!("Audio error: {error}"),
        );
    }

    PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!("Audio error: {error}"),
    )
}

/// Возвращает срез audio samples, который начинается не раньше текущей media clock base.
fn trim_decoded_audio_to_clock_base(
    samples: &[f32],
    packet_pts: Duration,
    media_clock_base: Duration,
    sample_rate: u32,
    channels: u32,
) -> &[f32] {
    if packet_pts >= media_clock_base || sample_rate == 0 || channels == 0 {
        return samples;
    }

    let channel_count = channels as usize;
    let frame_count = samples.len() / channel_count;
    if frame_count == 0 {
        return &[];
    }

    let trim_duration = media_clock_base.saturating_sub(packet_pts);
    let trim_frames = duration_to_audio_frames(trim_duration, sample_rate);
    if trim_frames >= frame_count {
        return &[];
    }

    let trim_samples = trim_frames.saturating_mul(channel_count);
    &samples[trim_samples..]
}

/// Конвертирует duration в количество audio frames с округлением вниз.
fn duration_to_audio_frames(duration: Duration, sample_rate: u32) -> usize {
    let frames = duration.as_nanos().saturating_mul(u128::from(sample_rate)) / 1_000_000_000u128;

    frames.min(usize::MAX as u128) as usize
}
