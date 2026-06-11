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

use std::cmp::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use tracing::{info, warn};

use crate::clock::AudioClock;
use crate::devices::{DEFAULT_AUDIO_OUTPUT_DEVICE_ID, output_device_from_stable_id};

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

    /// Громкость playback. 0.0 = silence, 1.0 = исходная амплитуда.
    volume: f32,

    /// Флаг: stream запущен или нет.
    is_playing: bool,

    /// Linear resampler для случая, когда decoder rate отличается от output rate.
    resampler: Option<LinearResampler>,

    /// Последнее поколение seek, для которого audio buffer был очищен.
    clear_ack_generation: u64,
}

/// Состояние linear resampling между decoded audio packets.
///
/// Opus отдаёт audio chunks пакетами, а output device может работать не на 48 kHz.
/// Чтобы на границе packets не было слышимого скачка, ресемплер хранит последний
/// source frame предыдущего packet-а и продолжает fractional позицию на следующем.
struct LinearResampler {
    /// Sample rate декодированного audio.
    source_rate: u32,

    /// Sample rate output stream.
    output_rate: u32,

    /// Количество interleaved каналов в stream layout.
    channel_count: usize,

    /// Следующая source-позиция относительно начала нового input chunk.
    next_source_frame_offset: f64,

    /// Последний frame предыдущего input chunk для интерполяции через boundary.
    previous_source_frame: Vec<f32>,
}

/// Результат низкоуровневой попытки остановить CPAL stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseStreamOutcome {
    /// Backend подтвердил физическую pause операцию.
    Paused,
    /// Backend не умеет hardware pause; для player-а это штатный logical pause.
    UnsupportedByBackend,
}

/// Политика обработки ошибки `StreamTrait::pause`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseErrorPolicy {
    /// Ошибка означает, что backend не поддерживает pause, но stream остаётся usable.
    NonFatalUnsupportedOperation,
    /// Ошибка означает реальную проблему stream/device и должна выйти наружу.
    Fatal,
}

impl LinearResampler {
    /// Создаёт linear resampler с явным соотношением частот.
    fn new(source_rate: u32, output_rate: u32, channel_count: usize) -> Self {
        Self {
            source_rate,
            output_rate,
            channel_count: channel_count.max(1),
            next_source_frame_offset: 0.0,
            previous_source_frame: Vec::new(),
        }
    }

    /// Возвращает шаг source frames на один output frame.
    fn source_frames_per_output_frame(&self) -> f64 {
        self.source_rate as f64 / self.output_rate as f64
    }

    /// Делает linear resample interleaved samples без смешивания каналов.
    fn resample_interleaved(&mut self, source_samples: &[f32]) -> Vec<f32> {
        if self.source_rate == 0 || self.output_rate == 0 {
            return Vec::new();
        }

        let source_frame_count = source_samples.len() / self.channel_count;
        if source_frame_count == 0 {
            return Vec::new();
        }

        let source_samples = &source_samples[..source_frame_count * self.channel_count];
        let carry_frame_count = usize::from(!self.previous_source_frame.is_empty());
        let combined_frame_count = source_frame_count + carry_frame_count;
        let mut combined_samples = Vec::with_capacity(combined_frame_count * self.channel_count);

        combined_samples.extend_from_slice(&self.previous_source_frame);
        combined_samples.extend_from_slice(source_samples);

        let mut source_frame_index =
            (self.next_source_frame_offset + carry_frame_count as f64).max(0.0);
        let source_frame_step = self.source_frames_per_output_frame();
        let mut resampled_samples = Vec::new();

        while source_frame_index.is_finite()
            && (source_frame_index as usize) + 1 < combined_frame_count
        {
            let frame_index = source_frame_index as usize;
            let frame_fraction = source_frame_index - frame_index as f64;

            for channel_index in 0..self.channel_count {
                let current_sample =
                    combined_samples[frame_index * self.channel_count + channel_index] as f64;
                let next_sample =
                    combined_samples[(frame_index + 1) * self.channel_count + channel_index] as f64;
                let interpolated_sample =
                    current_sample + frame_fraction * (next_sample - current_sample);
                resampled_samples.push(interpolated_sample as f32);
            }

            source_frame_index += source_frame_step;
        }

        self.next_source_frame_offset =
            source_frame_index - carry_frame_count as f64 - source_frame_count as f64;
        self.remember_last_source_frame(source_samples, source_frame_count);

        resampled_samples
    }

    /// Запоминает последний complete frame текущего chunk для следующего boundary.
    fn remember_last_source_frame(&mut self, source_samples: &[f32], source_frame_count: usize) {
        let last_frame_start = (source_frame_count - 1) * self.channel_count;
        let last_frame_end = last_frame_start + self.channel_count;

        self.previous_source_frame.clear();
        self.previous_source_frame
            .extend_from_slice(&source_samples[last_frame_start..last_frame_end]);
    }

    /// Сбрасывает carry state, чтобы после seek не смешивать старый и новый audio chunks.
    fn reset(&mut self) {
        self.next_source_frame_offset = 0.0;
        self.previous_source_frame.clear();
    }
}

/// Проверяет, что sample format можно отдать в typed CPAL output callback.
///
/// CPAL 0.15.3 объявляет `SampleFormat` как `non_exhaustive`, поэтому wildcard
/// остаётся явной защитой от будущих форматов, которые нельзя silently принять.
fn output_sample_format_is_supported(sample_format: SampleFormat) -> bool {
    matches!(
        sample_format,
        SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I32
            | SampleFormat::I64
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U32
            | SampleFormat::U64
            | SampleFormat::F32
            | SampleFormat::F64
    )
}

/// Даёт стабильный приоритет fallback-формату, когда default config unusable.
///
/// Обычно используется `default_output_config`; этот порядок нужен только для
/// редкого fallback path-а через `supported_output_configs`.
fn sample_format_priority(sample_format: SampleFormat) -> u8 {
    match sample_format {
        SampleFormat::F64 => 100,
        SampleFormat::F32 => 95,
        SampleFormat::I64 => 90,
        SampleFormat::I32 => 80,
        SampleFormat::I16 => 70,
        SampleFormat::I8 => 60,
        SampleFormat::U64 => 50,
        SampleFormat::U32 => 40,
        SampleFormat::U16 => 30,
        SampleFormat::U8 => 20,
        _ => 0,
    }
}

/// Проверяет, попадает ли желаемая частота в range CPAL config-а.
fn sample_rate_is_supported(
    config_range: &cpal::SupportedStreamConfigRange,
    sample_rate: cpal::SampleRate,
) -> bool {
    config_range.min_sample_rate() <= sample_rate && sample_rate <= config_range.max_sample_rate()
}

/// Сравнивает два supported ranges для fallback config selection.
fn compare_output_config_ranges(
    left: &cpal::SupportedStreamConfigRange,
    right: &cpal::SupportedStreamConfigRange,
    preferred_sample_rate: Option<cpal::SampleRate>,
) -> Ordering {
    let stereo_order = (left.channels() == 2).cmp(&(right.channels() == 2));
    if stereo_order != Ordering::Equal {
        return stereo_order;
    }

    let mono_order = (left.channels() == 1).cmp(&(right.channels() == 1));
    if mono_order != Ordering::Equal {
        return mono_order;
    }

    let channel_order = left.channels().cmp(&right.channels());
    if channel_order != Ordering::Equal {
        return channel_order;
    }

    let format_order = sample_format_priority(left.sample_format())
        .cmp(&sample_format_priority(right.sample_format()));
    if format_order != Ordering::Equal {
        return format_order;
    }

    let preferred_rate_order = preferred_sample_rate
        .map(|sample_rate| {
            sample_rate_is_supported(left, sample_rate)
                .cmp(&sample_rate_is_supported(right, sample_rate))
        })
        .unwrap_or(Ordering::Equal);
    if preferred_rate_order != Ordering::Equal {
        return preferred_rate_order;
    }

    left.max_sample_rate().cmp(&right.max_sample_rate())
}

/// Превращает supported range в concrete config без panic на sample rate.
fn config_from_supported_range(
    config_range: cpal::SupportedStreamConfigRange,
    preferred_sample_rate: Option<cpal::SampleRate>,
) -> cpal::SupportedStreamConfig {
    if let Some(sample_rate) = preferred_sample_rate {
        if let Some(config) = config_range.try_with_sample_rate(sample_rate) {
            return config;
        }
    }

    config_range.with_max_sample_rate()
}

/// Выбирает fallback output config только из форматов, которые умеет `AudioOutput`.
fn select_supported_output_config<I>(
    supported_ranges: I,
    preferred_sample_rate: Option<cpal::SampleRate>,
) -> Option<cpal::SupportedStreamConfig>
where
    I: IntoIterator<Item = cpal::SupportedStreamConfigRange>,
{
    supported_ranges
        .into_iter()
        .filter(|config_range| output_sample_format_is_supported(config_range.sample_format()))
        .max_by(|left, right| compare_output_config_ranges(left, right, preferred_sample_rate))
        .map(|config_range| config_from_supported_range(config_range, preferred_sample_rate))
}

/// Возвращает default output config или безопасный supported fallback.
fn choose_supported_output_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig> {
    let default_config = match device.default_output_config() {
        Ok(config) => Some(config),
        Err(error) => {
            warn!(error = %error, "CPAL default output config недоступен, пробуем supported list");
            None
        }
    };

    if let Some(config) = default_config
        .as_ref()
        .filter(|config| output_sample_format_is_supported(config.sample_format()))
    {
        return Ok(config.clone());
    }

    let preferred_sample_rate = default_config.as_ref().map(|config| config.sample_rate());
    let supported_ranges = device
        .supported_output_configs()
        .context("Не удалось получить supported output configs")?;
    let fallback_error_context = match default_config.as_ref() {
        Some(config) => format!(
            "Default CPAL output format {:?} не поддерживается AudioOutput, fallback не найден",
            config.sample_format()
        ),
        None => "CPAL не вернул ни одного поддерживаемого output config".to_string(),
    };
    let fallback_config = select_supported_output_config(supported_ranges, preferred_sample_rate)
        .context(fallback_error_context)?;

    if let Some(config) = default_config {
        warn!(
            default_format = ?config.sample_format(),
            fallback_format = ?fallback_config.sample_format(),
            fallback_rate = fallback_config.sample_rate().0,
            fallback_channels = fallback_config.channels(),
            "Default CPAL output config заменён supported fallback"
        );
    } else {
        info!(
            format = ?fallback_config.sample_format(),
            rate = fallback_config.sample_rate().0,
            channels = fallback_config.channels(),
            "Выбран CPAL output config из supported list"
        );
    }

    Ok(fallback_config)
}

/// Нормализует decoder sample перед CPAL conversion.
fn normalize_decoder_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Конвертирует внутренний f32 sample в concrete CPAL stream sample.
fn convert_sample_for_output<T>(sample: f32) -> T
where
    T: Sample + FromSample<f32>,
{
    T::from_sample(normalize_decoder_sample(sample))
}

/// Классифицирует pause error по CPAL 0.15 contract.
///
/// В 0.15.3 нет отдельного typed `UnsupportedOperation`, поэтому backend может
/// спрятать такую причину в `BackendSpecific.description`.
fn classify_pause_error(error: &cpal::PauseStreamError) -> PauseErrorPolicy {
    match error {
        cpal::PauseStreamError::DeviceNotAvailable => PauseErrorPolicy::Fatal,
        cpal::PauseStreamError::BackendSpecific { err } => {
            if backend_pause_error_is_unsupported_operation(&err.description) {
                PauseErrorPolicy::NonFatalUnsupportedOperation
            } else {
                PauseErrorPolicy::Fatal
            }
        }
    }
}

/// Распознаёт распространённые backend descriptions для неподдерживаемой pause.
fn backend_pause_error_is_unsupported_operation(description: &str) -> bool {
    let normalized_description = description.to_ascii_lowercase();

    normalized_description.contains("unsupportedoperation")
        || normalized_description.contains("unsupported operation")
        || normalized_description.contains("operation is not supported")
        || normalized_description.contains("operation not supported")
        || normalized_description.contains("not supported")
}

/// Останавливает CPAL stream с non-fatal policy для unsupported pause.
fn pause_stream_with_policy(stream: &cpal::Stream) -> Result<PauseStreamOutcome> {
    match stream.pause() {
        Ok(()) => Ok(PauseStreamOutcome::Paused),
        Err(error)
            if classify_pause_error(&error) == PauseErrorPolicy::NonFatalUnsupportedOperation =>
        {
            Ok(PauseStreamOutcome::UnsupportedByBackend)
        }
        Err(error) => Err(error).context("Не удалось остановить audio stream"),
    }
}

impl AudioOutput {
    /// Создаёт audio output на системном output device по умолчанию.
    pub fn new(decoder_rate: u32, decoder_channels: u32) -> Result<Self> {
        Self::new_with_device_id(
            decoder_rate,
            decoder_channels,
            DEFAULT_AUDIO_OUTPUT_DEVICE_ID,
        )
    }

    /// Создаёт audio output на выбранном stable device id.
    ///
    /// decoder_rate — sample rate декодера (48000 для Opus).
    /// decoder_channels — количество каналов декодера.
    pub fn new_with_device_id(
        decoder_rate: u32,
        decoder_channels: u32,
        output_device_id: &str,
    ) -> Result<Self> {
        if decoder_channels == 0 {
            anyhow::bail!("Количество каналов декодера не может быть 0");
        }

        let host = cpal::default_host();
        let device = output_device_from_stable_id(&host, output_device_id)
            .with_context(|| format!("Audio output device `{output_device_id}` is unavailable"))?;

        let output_config = choose_supported_output_config(&device)?;

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
            volume: 1.0,
            is_playing: false,
            resampler: needs_resample
                .then(|| LinearResampler::new(decoder_rate, stream_rate, stream_channels)),
            clear_ack_generation: 0,
        })
    }

    /// Записывает samples в ring buffer.
    ///
    /// Если decoder_rate != stream_rate, делает простой linear resample.
    /// Возвращает количество записанных samples.
    pub fn write_samples(&mut self, samples: &[f32]) -> u64 {
        if samples.is_empty() {
            return 0;
        }

        let stream_layout_samples = self.convert_decoder_samples_to_stream_layout(samples);
        let samples_for_output = match self.resampler.as_mut() {
            Some(resampler) => resampler.resample_interleaved(&stream_layout_samples),
            None => stream_layout_samples,
        };

        let mut written = 0u64;
        for sample in samples_for_output {
            if self.producer.try_push(sample * self.volume).is_err() {
                break;
            }
            written += 1;
        }

        if written > 0 {
            self.clock.record_written(written);
        }

        written
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
        let cleared_samples = self
            .consumer
            .lock()
            .map_err(|error| anyhow::anyhow!("Audio buffer mutex poisoned: {error}"))?
            .clear();

        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
        self.clock.reset();
        self.clear_ack_generation = generation;
        tracing::debug!(generation, cleared_samples, "Audio buffer cleared for seek");

        Ok(self.clear_ack_generation)
    }

    /// Возвращает последнее подтверждённое поколение очистки audio buffer.
    pub fn clear_ack_generation(&self) -> u64 {
        self.clear_ack_generation
    }

    /// Запускает playback.
    pub fn play(&mut self) -> Result<()> {
        if self.is_playing {
            return Ok(());
        }
        self.stream
            .play()
            .context("Не удалось запустить audio stream")?;
        self.is_playing = true;
        info!("Audio stream started");
        Ok(())
    }

    /// Останавливает playback.
    pub fn pause(&mut self) -> Result<()> {
        if !self.is_playing {
            return Ok(());
        }
        let pause_outcome = pause_stream_with_policy(&self.stream)?;
        self.is_playing = false;
        match pause_outcome {
            PauseStreamOutcome::Paused => {
                info!("Audio stream paused");
            }
            PauseStreamOutcome::UnsupportedByBackend => {
                warn!("CPAL backend не поддерживает pause; AudioOutput переходит в logical pause");
            }
        }
        Ok(())
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

    /// Преобразует interleaved samples декодера в interleaved layout output stream.
    fn convert_decoder_samples_to_stream_layout(&self, decoder_samples: &[f32]) -> Vec<f32> {
        let decoder_frame_count = decoder_samples.len() / self.decoder_channels;
        let mut stream_samples = Vec::with_capacity(decoder_frame_count * self.stream_channels);

        for decoder_frame in decoder_samples
            .chunks_exact(self.decoder_channels)
            .take(decoder_frame_count)
        {
            for stream_channel_index in 0..self.stream_channels {
                let sample = if self.decoder_channels == self.stream_channels {
                    decoder_frame[stream_channel_index]
                } else if self.decoder_channels == 1 {
                    decoder_frame[0]
                } else if stream_channel_index < self.decoder_channels {
                    decoder_frame[stream_channel_index]
                } else {
                    // Для дополнительных каналов устройства используем downmix L/R.
                    let sum: f32 = decoder_frame.iter().copied().sum();
                    sum / self.decoder_channels as f32
                };
                stream_samples.push(sample);
            }
        }

        stream_samples
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
        _channels: usize,
        callback_info: &cpal::OutputCallbackInfo,
        callback_observed_at: Instant,
    ) where
        T: Sample + FromSample<f32>,
    {
        let mut filled = 0u64;
        let mut silence = 0u64;

        match consumer.lock() {
            Ok(mut consumer) => {
                for sample in data.iter_mut() {
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
            }
            Err(_) => {
                data.fill(T::EQUILIBRIUM);
                silence = data.len() as u64;
            }
        }

        clock.record_output_callback(filled, silence, callback_info, callback_observed_at);
        if filled == 0 && silence > 0 {
            tracing::debug!(
                silence,
                total = data.len(),
                "CPAL callback: buffer underrun"
            );
        }
    }
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

    use cpal::{
        BackendSpecificError, FromSample, PauseStreamError, Sample, SampleFormat, SampleRate,
        SupportedBufferSize, SupportedStreamConfigRange,
    };

    use super::{
        LinearResampler, PauseErrorPolicy, classify_pause_error, convert_sample_for_output,
        output_sample_format_is_supported, sample_format_priority, select_supported_output_config,
    };

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
