//! Audio output — CPAL stream + ring buffer для playback.
//!
//! Архитектура:
//! - Main thread: decode → write_samples() → ring buffer producer
//! - CPAL callback: ring buffer consumer → fill output buffer
//! - При buffer underrun: заполнение silence (0.0)
//!
//! Ring buffer: lock-free SPSC (single-producer, single-consumer).
//!
//! ВАЖНО: используем default sample rate устройства чтобы избежать
//! проблем с ресемплингом на стороне ALSA/PipeWire. Opus декодирует
//! в 48kHz, поэтому мы делаем простой linear resample если device
//! использует другой rate.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use audio_core::{
    AudioChannelLayout, AudioOutputInputFrameCount, AudioOutputStreamFrameCount,
    AudioOutputWriteError, AudioOutputWriteIntent, AudioOutputWriteReport,
};
use cpal::traits::DeviceTrait;
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use tracing::{info, warn};

use crate::channel_mixer::ChannelMixer;
use crate::clock::AudioClock;
use crate::devices::{DEFAULT_AUDIO_OUTPUT_DEVICE_ID, output_device_from_stable_id};

mod configuration;
mod lifecycle;
mod processing;
mod resampler;

use configuration::choose_supported_output_config;
#[cfg(test)]
use configuration::{
    output_sample_format_is_supported, sample_format_priority, select_supported_output_config,
};
use lifecycle::pause_stream_with_policy;
#[cfg(test)]
use lifecycle::{PauseErrorPolicy, classify_pause_error};
#[cfg(test)]
use processing::{PEAK_LIMITER_CEILING, soft_clip_sample};
use processing::{
    advance_limiter_envelope, convert_sample_for_output, limiter_gain_for_envelope,
    limiter_release_decay_for_rate, output_sample_for_intent, reset_output_protection,
};
use resampler::LinearResampler;

/// Аудиовыход с CPAL stream и ring buffer.
pub struct AudioOutput {
    /// CPAL audio stream.
    stream: cpal::Stream,

    /// Producer half ring buffer.
    producer: HeapProd<f32>,

    /// Consumer half ring buffer, shared with CPAL callback for explicit seek clears.
    consumer: Arc<Mutex<HeapCons<f32>>>,

    /// Общий clock для A/V sync.
    clock: Arc<AudioClock>,

    /// Количество каналов output stream.
    stream_channels: usize,

    /// Количество каналов decoded audio.
    decoder_channels: usize,

    /// Предвычисленная layout-aware политика channel conversion.
    channel_mixer: ChannelMixer,

    /// Reusable storage channel mixer-а без packet-local allocation.
    channel_mix_buffer: Vec<f32>,

    /// Громкость playback. 0.0 = silence, 1.0 = исходная амплитуда.
    volume: f32,

    /// Флаг: stream запущен или нет.
    is_playing: bool,

    /// Фактическое состояние backend stream-а отдельно от logical playback.
    backend_stream_state: BackendStreamState,

    /// Linear resampler для случая, когда decoder rate отличается от output rate.
    resampler: Option<LinearResampler>,

    /// Огибающая пик-лимитера (мгновенная атака, экспоненциальный release).
    limiter_envelope: f32,

    /// Per-frame затухание огибающей лимитера, посчитанное от stream rate.
    limiter_release_decay: f32,

    /// Последнее поколение seek, для которого audio buffer был очищен.
    clear_ack_generation: u64,
}

/// Результат низкоуровневой попытки остановить CPAL stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseStreamOutcome {
    /// Backend подтвердил физическую pause операцию.
    Paused,
    /// Backend не умеет hardware pause; для player-а это штатный logical pause.
    UnsupportedByBackend,
}

/// Физическое состояние backend stream-а, не смешанное с logical pause gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendStreamState {
    /// Backend stream остановлен и требует `Stream::play` для resume.
    Paused,
    /// Backend продолжает callbacks; logical pause удерживается clock gate-ом.
    Running,
}

impl AudioOutput {
    /// Создаёт audio output на системном output device по умолчанию.
    pub fn new(decoder_rate: u32, decoder_layout: AudioChannelLayout) -> Result<Self> {
        Self::new_with_device_id(decoder_rate, decoder_layout, DEFAULT_AUDIO_OUTPUT_DEVICE_ID)
    }

    /// Создаёт audio output на выбранном stable device id.
    ///
    /// decoder_rate — sample rate декодера (48000 для Opus).
    /// decoder_layout — authoritative layout interleaved decoded PCM.
    pub fn new_with_device_id(
        decoder_rate: u32,
        decoder_layout: AudioChannelLayout,
        output_device_id: &str,
    ) -> Result<Self> {
        let decoder_channels = decoder_layout.channel_count();
        if decoder_channels == 0 {
            anyhow::bail!("Количество каналов декодера не может быть 0");
        }

        let host = cpal::default_host();
        let device = output_device_from_stable_id(&host, output_device_id)
            .with_context(|| format!("Audio output device `{output_device_id}` is unavailable"))?;

        let output_config = choose_supported_output_config(&device, decoder_rate)?;

        let device_name = device.name().unwrap_or_else(|_| "unknown".to_string());
        info!(
            stable_id = %output_device_id,
            device = %device_name,
            stream_rate = output_config.sample_rate().0,
            stream_channels = output_config.channels(),
            format = ?output_config.sample_format(),
            "Audio output device"
        );

        // Определяем stream rate и channels.
        // Обычно это default config устройства; fallback берётся только если
        // default format не подходит под typed CPAL callback-и AudioOutput.
        let stream_rate = output_config.sample_rate().0;
        let stream_channels = output_config.channels() as usize;
        let decoder_channels = decoder_channels as usize;

        // Если rate декодера отличается от stream rate, нужен ресемплинг.
        let needs_resample = stream_rate != decoder_rate;
        if needs_resample {
            info!(decoder_rate, stream_rate, "Будет использоваться ресемплинг");
        }

        info!(
            rate = stream_rate,
            channels = stream_channels,
            format = ?output_config.sample_format(),
            "CPAL stream config"
        );

        // Ring buffer capacity: ~2 seconds audio.
        let buffer_capacity = (stream_rate * stream_channels as u32 * 2) as usize;
        let rb = HeapRb::<f32>::new(buffer_capacity);
        let (producer, consumer) = rb.split();
        let consumer = Arc::new(Mutex::new(consumer));

        let clock = Arc::new(AudioClock::new(stream_rate, stream_channels as u32));
        let clock_for_callback = Arc::clone(&clock);

        // Получаем sample format и StreamConfig.
        let sample_format = output_config.sample_format();
        let stream_config = output_config.config();

        // Создаём stream в зависимости от sample format.
        let stream = Self::build_stream_for_sample_format(
            &device,
            &stream_config,
            sample_format,
            Arc::clone(&consumer),
            clock_for_callback,
            stream_channels,
        )?;

        info!(buffer_capacity, "AudioOutput создан");

        Ok(Self {
            stream,
            producer,
            consumer,
            clock,
            stream_channels,
            decoder_channels,
            channel_mixer: ChannelMixer::new(decoder_layout, stream_channels),
            channel_mix_buffer: Vec::new(),
            volume: 1.0,
            is_playing: false,
            backend_stream_state: BackendStreamState::Paused,
            resampler: needs_resample
                .then(|| LinearResampler::new(decoder_rate, stream_rate, stream_channels)),
            limiter_envelope: 0.0,
            limiter_release_decay: limiter_release_decay_for_rate(stream_rate),
            clear_ack_generation: 0,
        })
    }

    /// Записывает samples в ring buffer.
    ///
    /// Если decoder_rate != stream_rate, делает простой linear resample.
    /// Tempo-output проходит через limiter/soft-clip: overlap-add может
    /// превышать full scale, а device/BT-квантование превращает over-range
    /// в слышимый треск. Direct decoded PCM остаётся bit-transparent при
    /// совпадающем формате, `volume = 1.0` и диапазоне `[-1.0, 1.0]`.
    /// Возвращает typed frame accounting до и после concrete преобразований.
    pub fn write_samples(
        &mut self,
        samples: &[f32],
        intent: AudioOutputWriteIntent,
    ) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError> {
        let submitted_input_frames =
            audio_output_input_frame_count(samples.len(), self.decoder_channels)?;

        if intent == AudioOutputWriteIntent::DirectDecodedPcm {
            // Direct PCM не использует protection state. Сброс не позволяет
            // старому tempo peak позднее приглушить новый tempo segment.
            reset_output_protection(&mut self.limiter_envelope);
        }

        if samples.is_empty() {
            return Ok(AudioOutputWriteReport::complete(
                submitted_input_frames,
                AudioOutputStreamFrameCount::new(0),
            ));
        }

        let stream_layout_samples = self
            .channel_mixer
            .mix_interleaved_into(samples, &mut self.channel_mix_buffer)?;
        let resampled_samples = self
            .resampler
            .as_mut()
            .map(|resampler| resampler.resample_interleaved(stream_layout_samples));
        let samples_for_output = resampled_samples
            .as_deref()
            .unwrap_or(stream_layout_samples);

        // Ring buffer обязан оставаться frame-aligned: запись, оборванная
        // посреди interleaved кадра, навсегда меняет каналы местами для всего
        // последующего потока (правый канал играет слева).
        let channels = self.stream_channels.max(1);
        let vacant = self.producer.vacant_len();
        let frame_aligned_vacant = vacant - (vacant % channels);
        let writable = samples_for_output.len().min(frame_aligned_vacant);

        let requested = samples_for_output.len();
        let mut written = 0usize;
        // Пик-лимитер tempo-output работает по кадрам: общая для каналов
        // огибающая не сдвигает стереообраз. Direct PCM обходит этот branch.
        'frames: for frame in samples_for_output[..writable].chunks_exact(channels) {
            let limiter_gain = match intent {
                AudioOutputWriteIntent::DirectDecodedPcm => 1.0,
                AudioOutputWriteIntent::TempoProcessed => {
                    let frame_peak = frame.iter().fold(0.0f32, |peak, sample| {
                        peak.max((sample * self.volume).abs())
                    });
                    self.limiter_envelope = advance_limiter_envelope(
                        self.limiter_envelope,
                        frame_peak,
                        self.limiter_release_decay,
                    );
                    limiter_gain_for_envelope(self.limiter_envelope)
                }
            };

            for sample in frame {
                let protected_sample =
                    output_sample_for_intent(*sample, self.volume, limiter_gain, intent);
                if self.producer.try_push(protected_sample).is_err() {
                    break 'frames;
                }
                written += 1;
            }
        }

        if written < requested {
            // Потерянные samples — это слышимый разрыв и разрыв continuity
            // clock mapping; потеря обязана быть видимой, а не молчаливой.
            warn!(
                requested,
                written,
                dropped = requested - written,
                "Audio ring buffer переполнен: часть samples отброшена"
            );
        }

        let write_report = build_audio_output_write_report(
            submitted_input_frames,
            requested,
            written,
            self.stream_channels,
        )?;

        if written > 0 {
            self.clock.record_written(written as u64);
        }

        Ok(write_report)
    }

    /// Устанавливает громкость для последующих samples.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    /// Возвращает reference на audio clock.
    pub fn clock(&self) -> &Arc<AudioClock> {
        &self.clock
    }

    /// Очищает queued audio samples и подтверждает seek generation.
    ///
    /// Метод синхронный: player-core может продолжать seek commit только после
    /// возврата ack, поэтому старый звук не остаётся в ring buffer.
    pub fn clear_buffer_for_seek(&mut self, generation: u64) -> Result<u64> {
        let mut consumer = self
            .consumer
            .lock()
            .map_err(|error| anyhow::anyhow!("Audio buffer mutex poisoned: {error}"))?;
        let cleared_samples = consumer.clear();

        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
        reset_output_protection(&mut self.limiter_envelope);
        // Consumer lock остаётся захваченным до reset: callback не сможет
        // опубликовать старый playback anchor после возврата seek clear ack.
        self.clock.reset();
        self.clear_ack_generation = generation;
        drop(consumer);
        tracing::debug!(generation, cleared_samples, "Audio buffer cleared for seek");

        Ok(self.clear_ack_generation)
    }

    /// Возвращает последнее подтверждённое поколение очистки audio buffer.
    pub fn clear_ack_generation(&self) -> u64 {
        self.clear_ack_generation
    }

    /// Возвращает уровень заполнения buffer в миллисекундах.
    pub fn buffer_level_ms(&self) -> f64 {
        let level = self.clock.buffer_level() as f64;
        let channels = self.stream_channels as f64;
        let sample_rate = self.clock.sample_rate() as f64;
        if sample_rate == 0.0 || channels == 0.0 {
            return 0.0;
        }
        (level / channels / sample_rate) * 1000.0
    }

    /// Создаёт typed CPAL stream для выбранного runtime sample format.
    fn build_stream_for_sample_format(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        sample_format: SampleFormat,
        consumer: Arc<Mutex<HeapCons<f32>>>,
        clock: Arc<AudioClock>,
        channels: usize,
    ) -> Result<cpal::Stream> {
        match sample_format {
            SampleFormat::I8 => Self::build_stream::<i8>(device, config, consumer, clock, channels),
            SampleFormat::I16 => {
                Self::build_stream::<i16>(device, config, consumer, clock, channels)
            }
            SampleFormat::I32 => {
                Self::build_stream::<i32>(device, config, consumer, clock, channels)
            }
            SampleFormat::I64 => {
                Self::build_stream::<i64>(device, config, consumer, clock, channels)
            }
            SampleFormat::U8 => Self::build_stream::<u8>(device, config, consumer, clock, channels),
            SampleFormat::U16 => {
                Self::build_stream::<u16>(device, config, consumer, clock, channels)
            }
            SampleFormat::U32 => {
                Self::build_stream::<u32>(device, config, consumer, clock, channels)
            }
            SampleFormat::U64 => {
                Self::build_stream::<u64>(device, config, consumer, clock, channels)
            }
            SampleFormat::F32 => {
                Self::build_stream::<f32>(device, config, consumer, clock, channels)
            }
            SampleFormat::F64 => {
                Self::build_stream::<f64>(device, config, consumer, clock, channels)
            }
            other => anyhow::bail!("Unsupported sample format: {:?}", other),
        }
    }

    /// Создаёт CPAL stream для конкретного Rust sample type.
    fn build_stream<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        consumer: Arc<Mutex<HeapCons<f32>>>,
        clock: Arc<AudioClock>,
        channels: usize,
    ) -> Result<cpal::Stream>
    where
        T: SizedSample + Sample + FromSample<f32> + Send + 'static,
    {
        let err_callback = move |err| {
            warn!("CPAL error: {}", err);
        };

        let stream = device.build_output_stream(
            config,
            move |data: &mut [T], callback_info: &cpal::OutputCallbackInfo| {
                Self::fill_buffer(
                    data,
                    &consumer,
                    &clock,
                    channels,
                    callback_info,
                    Instant::now(),
                );
            },
            err_callback,
            None,
        )?;

        Ok(stream)
    }

    /// Заполняет output buffer из ring buffer для любого CPAL sample type.
    fn fill_buffer<T>(
        data: &mut [T],
        consumer: &Arc<Mutex<HeapCons<f32>>>,
        clock: &AudioClock,
        channels: usize,
        callback_info: &cpal::OutputCallbackInfo,
        callback_observed_at: Instant,
    ) where
        T: Sample + FromSample<f32>,
    {
        let mut filled = 0u64;
        let mut silence = 0u64;

        match consumer.lock() {
            Ok(mut consumer) => {
                // Logical pause нужен даже для backend-ов без CPAL pause:
                // callback выдаёт silence, но не расходует ring PCM и clock.
                if clock.output_timing_is_frozen() {
                    data.fill(T::EQUILIBRIUM);
                    return;
                }

                // Underrun не должен рвать interleaved кадр: чтение, оборванное
                // посреди кадра, сдвигает каналы всего последующего потока
                // (звук «прыгает» между левым и правым). Читаем целые кадры,
                // неполный хвост оставляем в ring buffer до следующего callback.
                let channels = channels.max(1);
                let occupied = consumer.occupied_len();
                let frame_aligned_occupied = occupied - (occupied % channels);
                let to_fill = data.len().min(frame_aligned_occupied);

                for sample in data[..to_fill].iter_mut() {
                    match consumer.try_pop() {
                        Some(value) => {
                            *sample = convert_sample_for_output(value);
                            filled += 1;
                        }
                        None => {
                            *sample = T::EQUILIBRIUM;
                            silence += 1;
                        }
                    }
                }
                for sample in data[to_fill..].iter_mut() {
                    *sample = T::EQUILIBRIUM;
                    silence += 1;
                }

                // Clock publication входит в ту же critical section, что и
                // consumer read. Seek clear поэтому либо увидит весь callback,
                // либо сбросит его целиком, но не получит поздний старый anchor.
                clock.record_output_callback(filled, silence, callback_info, callback_observed_at);
                drop(consumer);
            }
            Err(_) => {
                data.fill(T::EQUILIBRIUM);
                if clock.output_timing_is_frozen() {
                    return;
                }
                silence = data.len() as u64;
                clock.record_output_callback(filled, silence, callback_info, callback_observed_at);
            }
        }

        if filled == 0 && silence > 0 {
            tracing::debug!(
                silence,
                total = data.len(),
                "CPAL callback: buffer underrun"
            );
        }
    }
}

/// Переводит scalar input samples в typed input frames без потери хвоста.
fn audio_output_input_frame_count(
    input_samples: usize,
    input_channels: usize,
) -> std::result::Result<AudioOutputInputFrameCount, AudioOutputWriteError> {
    if input_channels == 0 {
        return Err(AudioOutputWriteError::InvalidChannelCount { boundary: "input" });
    }
    if !input_samples.is_multiple_of(input_channels) {
        return Err(AudioOutputWriteError::InputNotFrameAligned {
            input_samples,
            input_channels,
        });
    }

    Ok(AudioOutputInputFrameCount::new(
        input_samples / input_channels,
    ))
}

/// Строит neutral write report в frame units после conversion/resampling.
fn build_audio_output_write_report(
    submitted_input_frames: AudioOutputInputFrameCount,
    prepared_output_samples: usize,
    queued_output_samples: usize,
    output_channels: usize,
) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError> {
    if output_channels == 0 {
        return Err(AudioOutputWriteError::InvalidChannelCount { boundary: "output" });
    }
    if !prepared_output_samples.is_multiple_of(output_channels) {
        return Err(AudioOutputWriteError::PreparedOutputNotFrameAligned {
            prepared_output_samples,
            output_channels,
        });
    }
    if !queued_output_samples.is_multiple_of(output_channels) {
        return Err(AudioOutputWriteError::QueuedOutputNotFrameAligned {
            queued_output_samples,
            output_channels,
        });
    }

    AudioOutputWriteReport::try_new(
        submitted_input_frames,
        AudioOutputStreamFrameCount::new(prepared_output_samples / output_channels),
        AudioOutputStreamFrameCount::new(queued_output_samples / output_channels),
    )
}

/// Корректная остановка stream перед уничтожением.
impl Drop for AudioOutput {
    fn drop(&mut self) {
        match pause_stream_with_policy(&self.stream) {
            Ok(PauseStreamOutcome::Paused) => {}
            Ok(PauseStreamOutcome::UnsupportedByBackend) => {
                tracing::debug!(
                    "AudioOutput::drop — CPAL backend не поддерживает pause, stream будет остановлен drop-ом"
                );
            }
            Err(error) => {
                warn!(
                    "AudioOutput::drop — не удалось остановить stream: {}",
                    error
                );
            }
        }
        info!("AudioOutput остановлен (drop)");
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use audio_core::{
        AudioChannelLayout, AudioOutputInputFrameCount, AudioOutputStreamFrameCount,
        AudioOutputWriteError, AudioOutputWriteIntent,
    };
    use cpal::{
        BackendSpecificError, FromSample, PauseStreamError, Sample, SampleFormat, SampleRate,
        SupportedBufferSize, SupportedStreamConfigRange,
    };

    use super::{
        ChannelMixer, LinearResampler, PEAK_LIMITER_CEILING, PauseErrorPolicy,
        advance_limiter_envelope, audio_output_input_frame_count, build_audio_output_write_report,
        classify_pause_error, convert_sample_for_output, limiter_gain_for_envelope,
        limiter_release_decay_for_rate, output_sample_for_intent,
        output_sample_format_is_supported, reset_output_protection, sample_format_priority,
        select_supported_output_config, soft_clip_sample,
    };

    #[test]
    fn multichannel_to_stereo_write_accounting_is_complete_in_frame_domain() {
        let input_samples = vec![0.0_f32; 6_144];
        let mixer = ChannelMixer::new(AudioChannelLayout::surround_5_1(), 2);
        let mut stereo_samples = Vec::new();
        mixer
            .mix_interleaved_into(&input_samples, &mut stereo_samples)
            .expect("canonical 5.1 must downmix to stereo");
        assert_eq!(stereo_samples.len(), 2_048);

        let submitted_input_frames = audio_output_input_frame_count(6_144, 6)
            .expect("1024 complete 5.1 frames should be accepted");
        let report = build_audio_output_write_report(
            submitted_input_frames,
            stereo_samples.len(),
            stereo_samples.len(),
            2,
        )
        .expect("1024 prepared stereo frames should be fully queued");

        assert_eq!(
            report.submitted_input_frames(),
            AudioOutputInputFrameCount::new(1_024)
        );
        assert_eq!(
            report.prepared_output_frames(),
            AudioOutputStreamFrameCount::new(1_024)
        );
        assert_eq!(
            report.queued_output_frames(),
            AudioOutputStreamFrameCount::new(1_024)
        );
        assert!(report.is_complete());
    }

    #[test]
    fn resampled_complete_write_and_real_partial_keep_output_frame_units() {
        let submitted_input_frames = audio_output_input_frame_count(2_048, 2)
            .expect("1024 complete stereo input frames should be accepted");
        let complete_report =
            build_audio_output_write_report(submitted_input_frames, 2_230, 2_230, 2)
                .expect("resampled output can have a different complete frame count");
        assert_eq!(
            complete_report.prepared_output_frames(),
            AudioOutputStreamFrameCount::new(1_115)
        );
        assert!(complete_report.is_complete());

        let partial_report =
            build_audio_output_write_report(submitted_input_frames, 2_048, 2_046, 2)
                .expect("frame-aligned ring pressure should produce a typed partial report");
        assert_eq!(
            partial_report.queued_output_frames(),
            AudioOutputStreamFrameCount::new(1_023)
        );
        assert_eq!(
            partial_report.dropped_output_frames(),
            AudioOutputStreamFrameCount::new(1)
        );
        assert!(!partial_report.is_complete());
    }

    #[test]
    fn malformed_multichannel_input_is_rejected_before_conversion() {
        let error = audio_output_input_frame_count(6_143, 6)
            .expect_err("an interleaved 5.1 slice must end on a complete frame");

        assert_eq!(
            error,
            AudioOutputWriteError::InputNotFrameAligned {
                input_samples: 6_143,
                input_channels: 6,
            }
        );
    }

    #[test]
    fn peak_limiter_ducks_overshoot_and_releases_smoothly() {
        let release_decay = limiter_release_decay_for_rate(48_000);
        assert!(release_decay > 0.999 && release_decay < 1.0);

        // Обычный сигнал: gain единичный.
        let mut envelope = 0.0f32;
        envelope = advance_limiter_envelope(envelope, 0.5, release_decay);
        assert_eq!(limiter_gain_for_envelope(envelope), 1.0);

        // Пик 2.0 после tempo overlap-add: мгновенная атака давит его к потолку.
        envelope = advance_limiter_envelope(envelope, 2.0, release_decay);
        let ducked_gain = limiter_gain_for_envelope(envelope);
        assert!((2.0 * ducked_gain - PEAK_LIMITER_CEILING).abs() < 1e-3);

        // Release: gain восстанавливается монотонно, без ступеней.
        let mut previous_gain = ducked_gain;
        for _ in 0..48_000 {
            envelope = advance_limiter_envelope(envelope, 0.1, release_decay);
            let gain = limiter_gain_for_envelope(envelope);
            assert!(gain >= previous_gain);
            previous_gain = gain;
        }
        assert_eq!(
            previous_gain, 1.0,
            "через секунду gain должен вернуться к 1"
        );
    }

    #[test]
    fn soft_clip_passes_normal_signal_and_bounds_overshoot() {
        // Обычный сигнал до колена проходит бит-в-бит.
        assert_eq!(soft_clip_sample(0.0), 0.0);
        assert_eq!(soft_clip_sample(0.5), 0.5);
        assert_eq!(soft_clip_sample(-0.95), -0.95);

        // Over-range плавно насыщается ниже full scale, знак сохраняется.
        let clipped = soft_clip_sample(1.1);
        assert!(clipped > 0.95 && clipped < 1.0, "clipped={clipped}");
        assert_eq!(soft_clip_sample(-1.1), -clipped);

        // Монотонность выше колена: сильнее вход — не меньше выход,
        // асимптота — ровно full scale.
        assert!(soft_clip_sample(1.5) >= clipped);
        assert!(soft_clip_sample(10.0) <= 1.0);

        // Non-finite сэмплы не должны попадать в устройство.
        assert_eq!(soft_clip_sample(f32::NAN), 0.0);
        assert_eq!(soft_clip_sample(f32::INFINITY), 0.0);
    }

    #[test]
    fn direct_pcm_intent_is_bit_transparent_at_unity_volume() {
        let direct_samples = [-1.0f32, -0.95, -0.25, -0.0, 0.0, 0.25, 0.95, 1.0];

        for input_sample in direct_samples {
            let output_sample = output_sample_for_intent(
                input_sample,
                1.0,
                0.25,
                AudioOutputWriteIntent::DirectDecodedPcm,
            );

            assert_eq!(
                output_sample.to_bits(),
                input_sample.to_bits(),
                "direct PCM должен игнорировать tempo limiter gain"
            );
            assert_eq!(
                convert_sample_for_output::<f32>(output_sample).to_bits(),
                input_sample.to_bits(),
                "same-format CPAL conversion не должна менять normal PCM"
            );
        }
    }

    #[test]
    fn tempo_processed_intent_applies_existing_output_protection() {
        let overshoot = 1.4f32;
        let limiter_gain = limiter_gain_for_envelope(overshoot);

        let protected_sample = output_sample_for_intent(
            overshoot,
            1.0,
            limiter_gain,
            AudioOutputWriteIntent::TempoProcessed,
        );
        let direct_sample = output_sample_for_intent(
            overshoot,
            1.0,
            limiter_gain,
            AudioOutputWriteIntent::DirectDecodedPcm,
        );

        assert!(protected_sample.abs() <= 1.0);
        assert!(protected_sample.abs() < direct_sample.abs());
    }

    #[test]
    fn lifecycle_reset_clears_tempo_protection_history() {
        let mut limiter_envelope = 1.75f32;

        reset_output_protection(&mut limiter_envelope);

        assert_eq!(limiter_envelope, 0.0);
    }

    /// Проверяет floating-point samples с небольшим допуском.
    fn assert_samples_close(actual_samples: &[f32], expected_samples: &[f32]) {
        assert_eq!(actual_samples.len(), expected_samples.len());

        for (actual_sample, expected_sample) in actual_samples.iter().zip(expected_samples) {
            let delta = (actual_sample - expected_sample).abs();
            assert!(
                delta < 0.000_01,
                "sample mismatch: actual={actual_sample}, expected={expected_sample}"
            );
        }
    }

    /// Создаёт CPAL range без обращения к реальному audio device.
    fn output_config_range(
        sample_format: SampleFormat,
        channels: u16,
        min_sample_rate: u32,
        max_sample_rate: u32,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(
            channels,
            SampleRate(min_sample_rate),
            SampleRate(max_sample_rate),
            SupportedBufferSize::Unknown,
            sample_format,
        )
    }

    /// Проверяет conversion helper для конкретного target sample type.
    fn assert_converted_sample<T>(source_sample: f32, expected_sample: T)
    where
        T: Sample + FromSample<f32> + Debug,
    {
        assert_eq!(
            convert_sample_for_output::<T>(source_sample),
            expected_sample
        );
    }

    #[test]
    fn cpal_015_sample_formats_are_supported_by_output() {
        let cpal_015_sample_formats = [
            SampleFormat::I8,
            SampleFormat::I16,
            SampleFormat::I32,
            SampleFormat::I64,
            SampleFormat::U8,
            SampleFormat::U16,
            SampleFormat::U32,
            SampleFormat::U64,
            SampleFormat::F32,
            SampleFormat::F64,
        ];

        for sample_format in cpal_015_sample_formats {
            assert!(
                output_sample_format_is_supported(sample_format),
                "{sample_format:?} должен поддерживаться AudioOutput"
            );
            assert!(
                sample_format_priority(sample_format) > 0,
                "{sample_format:?} должен иметь fallback priority"
            );
        }
    }

    #[test]
    fn sample_conversion_clips_all_cpal_015_formats() {
        assert_converted_sample::<i8>(-1.0, i8::MIN);
        assert_converted_sample::<i8>(1.0, i8::MAX);
        assert_converted_sample::<i16>(-1.0, i16::MIN);
        assert_converted_sample::<i16>(1.0, i16::MAX);
        assert_converted_sample::<i32>(-1.0, i32::MIN);
        assert_converted_sample::<i32>(1.0, i32::MAX);
        assert_converted_sample::<i64>(-1.0, i64::MIN);
        assert_converted_sample::<i64>(1.0, i64::MAX);

        assert_converted_sample::<u8>(-1.0, u8::MIN);
        assert_converted_sample::<u8>(0.0, 1u8 << 7);
        assert_converted_sample::<u8>(1.0, u8::MAX);
        assert_converted_sample::<u16>(-1.0, u16::MIN);
        assert_converted_sample::<u16>(0.0, 1u16 << 15);
        assert_converted_sample::<u16>(1.0, u16::MAX);
        assert_converted_sample::<u32>(-1.0, u32::MIN);
        assert_converted_sample::<u32>(0.0, 1u32 << 31);
        assert_converted_sample::<u32>(1.0, u32::MAX);
        assert_converted_sample::<u64>(-1.0, u64::MIN);
        assert_converted_sample::<u64>(0.0, 1u64 << 63);
        assert_converted_sample::<u64>(1.0, u64::MAX);

        assert_converted_sample::<f32>(-2.0, -1.0);
        assert_converted_sample::<f32>(f32::NAN, 0.0);
        assert_converted_sample::<f64>(2.0, 1.0);
        assert_converted_sample::<f64>(f32::INFINITY, 0.0);
    }

    #[test]
    fn fallback_output_config_uses_supported_non_legacy_formats() {
        let selected_config = select_supported_output_config(
            [
                output_config_range(SampleFormat::U8, 2, 44_100, 96_000),
                output_config_range(SampleFormat::I32, 2, 44_100, 96_000),
            ],
            Some(SampleRate(48_000)),
        )
        .expect("supported fallback config");

        assert_eq!(selected_config.sample_format(), SampleFormat::I32);
        assert_eq!(selected_config.sample_rate(), SampleRate(48_000));
        assert_eq!(selected_config.channels(), 2);
    }

    #[test]
    fn fallback_output_config_uses_max_rate_when_preferred_rate_is_outside_range() {
        let selected_config = select_supported_output_config(
            [output_config_range(SampleFormat::F32, 2, 44_100, 48_000)],
            Some(SampleRate(96_000)),
        )
        .expect("supported fallback config");

        assert_eq!(selected_config.sample_format(), SampleFormat::F32);
        assert_eq!(selected_config.sample_rate(), SampleRate(48_000));
    }

    #[test]
    fn pause_unsupported_operation_is_non_fatal_policy() {
        let unsupported_error = PauseStreamError::BackendSpecific {
            err: BackendSpecificError {
                description: "UnsupportedOperation: pause is not supported".to_string(),
            },
        };
        let device_error = PauseStreamError::DeviceNotAvailable;

        assert_eq!(
            classify_pause_error(&unsupported_error),
            PauseErrorPolicy::NonFatalUnsupportedOperation
        );
        assert_eq!(classify_pause_error(&device_error), PauseErrorPolicy::Fatal);
    }

    #[test]
    fn resampler_interpolates_across_packet_boundary() {
        let mut resampler = LinearResampler::new(3, 4, 1);

        let first_output = resampler.resample_interleaved(&[0.0, 1.0]);
        let second_output = resampler.resample_interleaved(&[2.0, 3.0, 4.0]);

        assert_samples_close(&first_output, &[0.0, 0.75]);
        assert_samples_close(&second_output, &[1.5, 2.25, 3.0, 3.75]);
    }

    #[test]
    fn resampler_keeps_channels_separate_across_packet_boundary() {
        let mut resampler = LinearResampler::new(3, 4, 2);

        let first_output = resampler.resample_interleaved(&[0.0, 10.0, 1.0, 11.0]);
        let second_output = resampler.resample_interleaved(&[2.0, 12.0, 3.0, 13.0, 4.0, 14.0]);

        assert_samples_close(&first_output, &[0.0, 10.0, 0.75, 10.75]);
        assert_samples_close(
            &second_output,
            &[1.5, 11.5, 2.25, 12.25, 3.0, 13.0, 3.75, 13.75],
        );
    }
}
