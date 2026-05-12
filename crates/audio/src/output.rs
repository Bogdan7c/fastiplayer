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
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use tracing::{info, warn};

use crate::clock::AudioClock;

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

impl AudioOutput {
    /// Создаёт audio output.
    ///
    /// decoder_rate — sample rate декодера (48000 для Opus).
    /// decoder_channels — количество каналов декодера.
    pub fn new(decoder_rate: u32, decoder_channels: u32) -> Result<Self> {
        if decoder_channels == 0 {
            anyhow::bail!("Количество каналов декодера не может быть 0");
        }

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("No default audio output device found")?;

        let default_config = device
            .default_output_config()
            .context("Не удалось получить default output config")?;

        let device_name = device.name().unwrap_or_else(|_| "unknown".to_string());
        info!(
            device = %device_name,
            default_rate = default_config.sample_rate().0,
            default_channels = default_config.channels(),
            format = ?default_config.sample_format(),
            "Audio output device"
        );

        // Определяем stream rate и channels.
        // Берём default rate устройства — это гарантирует что ALSA/PipeWire
        // не будет делать свой ресемплинг.
        let stream_rate = default_config.sample_rate().0;
        let stream_channels = default_config.channels() as usize;
        let decoder_channels = decoder_channels as usize;

        // Если rate декодера отличается от stream rate, нужен ресемплинг.
        let needs_resample = stream_rate != decoder_rate;
        if needs_resample {
            info!(decoder_rate, stream_rate, "Будет использоваться ресемплинг");
        }

        info!(
            rate = stream_rate,
            channels = stream_channels,
            format = ?default_config.sample_format(),
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
        let sample_format = default_config.sample_format();
        let stream_config: cpal::StreamConfig = default_config.into();

        // Создаём stream в зависимости от sample format.
        let stream = match sample_format {
            cpal::SampleFormat::F32 => Self::build_stream_f32(
                &device,
                &stream_config,
                Arc::clone(&consumer),
                clock_for_callback,
                stream_channels,
            )?,
            cpal::SampleFormat::I16 => Self::build_stream_i16(
                &device,
                &stream_config,
                Arc::clone(&consumer),
                clock_for_callback,
                stream_channels,
            )?,
            cpal::SampleFormat::U16 => Self::build_stream_u16(
                &device,
                &stream_config,
                Arc::clone(&consumer),
                clock_for_callback,
                stream_channels,
            )?,
            other => {
                anyhow::bail!("Unsupported sample format: {:?}", other);
            }
        };

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
        self.stream
            .pause()
            .context("Не удалось остановить audio stream")?;
        self.is_playing = false;
        info!("Audio stream paused");
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

    /// Создаёт CPAL stream для f32 sample format.
    fn build_stream_f32(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        consumer: Arc<Mutex<HeapCons<f32>>>,
        clock: Arc<AudioClock>,
        channels: usize,
    ) -> Result<cpal::Stream> {
        let err_callback = move |err| {
            warn!("CPAL error: {}", err);
        };

        let stream = device.build_output_stream(
            config,
            move |data: &mut [f32], callback_info: &cpal::OutputCallbackInfo| {
                Self::fill_buffer_f32(
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

    /// Создаёт CPAL stream для i16 sample format.
    fn build_stream_i16(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        consumer: Arc<Mutex<HeapCons<f32>>>,
        clock: Arc<AudioClock>,
        channels: usize,
    ) -> Result<cpal::Stream> {
        let err_callback = move |err| {
            warn!("CPAL error: {}", err);
        };

        let stream = device.build_output_stream(
            config,
            move |data: &mut [i16], callback_info: &cpal::OutputCallbackInfo| {
                Self::fill_buffer_i16(
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

    /// Создаёт CPAL stream для u16 sample format.
    fn build_stream_u16(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        consumer: Arc<Mutex<HeapCons<f32>>>,
        clock: Arc<AudioClock>,
        channels: usize,
    ) -> Result<cpal::Stream> {
        let err_callback = move |err| {
            warn!("CPAL error: {}", err);
        };

        let stream = device.build_output_stream(
            config,
            move |data: &mut [u16], callback_info: &cpal::OutputCallbackInfo| {
                Self::fill_buffer_u16(
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

    /// Заполняет output buffer из ring buffer (f32).
    fn fill_buffer_f32(
        data: &mut [f32],
        consumer: &Arc<Mutex<HeapCons<f32>>>,
        clock: &AudioClock,
        _channels: usize,
        callback_info: &cpal::OutputCallbackInfo,
        callback_observed_at: Instant,
    ) {
        let mut filled = 0u64;
        let mut silence = 0u64;

        match consumer.lock() {
            Ok(mut consumer) => {
                for sample in data.iter_mut() {
                    match consumer.try_pop() {
                        Some(value) => {
                            *sample = value;
                            filled += 1;
                        }
                        None => {
                            *sample = 0.0;
                            silence += 1;
                        }
                    }
                }
            }
            Err(_) => {
                data.fill(0.0);
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

    /// Заполняет output buffer из ring buffer (i16).
    fn fill_buffer_i16(
        data: &mut [i16],
        consumer: &Arc<Mutex<HeapCons<f32>>>,
        clock: &AudioClock,
        _channels: usize,
        callback_info: &cpal::OutputCallbackInfo,
        callback_observed_at: Instant,
    ) {
        let mut filled = 0u64;
        let mut silence = 0u64;

        match consumer.lock() {
            Ok(mut consumer) => {
                for sample in data.iter_mut() {
                    match consumer.try_pop() {
                        Some(value) => {
                            let value_f64: f64 = value as f64;
                            *sample = (value_f64 * 32767.0).clamp(-32768.0, 32767.0) as i16;
                            filled += 1;
                        }
                        None => {
                            *sample = 0;
                            silence += 1;
                        }
                    }
                }
            }
            Err(_) => {
                data.fill(0);
                silence = data.len() as u64;
            }
        }

        clock.record_output_callback(filled, silence, callback_info, callback_observed_at);
    }

    /// Заполняет output buffer из ring buffer (u16).
    fn fill_buffer_u16(
        data: &mut [u16],
        consumer: &Arc<Mutex<HeapCons<f32>>>,
        clock: &AudioClock,
        _channels: usize,
        callback_info: &cpal::OutputCallbackInfo,
        callback_observed_at: Instant,
    ) {
        let mut filled = 0u64;
        let mut silence = 0u64;

        match consumer.lock() {
            Ok(mut consumer) => {
                for sample in data.iter_mut() {
                    match consumer.try_pop() {
                        Some(value) => {
                            let value_f64: f64 = value as f64;
                            *sample =
                                ((value_f64 * 0.5 + 0.5) * 65535.0).clamp(0.0, 65535.0) as u16;
                            filled += 1;
                        }
                        None => {
                            *sample = 32768;
                            silence += 1;
                        }
                    }
                }
            }
            Err(_) => {
                data.fill(32768);
                silence = data.len() as u64;
            }
        }

        clock.record_output_callback(filled, silence, callback_info, callback_observed_at);
    }
}

/// Корректная остановка stream перед уничтожением.
impl Drop for AudioOutput {
    fn drop(&mut self) {
        if let Err(e) = self.stream.pause() {
            warn!("AudioOutput::drop — не удалось остановить stream: {}", e);
        }
        info!("AudioOutput остановлен (drop)");
    }
}

#[cfg(test)]
mod tests {
    use super::LinearResampler;

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
