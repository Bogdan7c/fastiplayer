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

use std::sync::Arc;

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

    /// Общий clock для A/V sync.
    clock: Arc<AudioClock>,

    /// Количество каналов output stream.
    stream_channels: usize,

    /// Количество каналов decoded audio.
    decoder_channels: usize,

    /// Sample rate декодированного audio (например 48000 для Opus).
    decoder_rate: u32,

    /// Sample rate output stream (может отличаться от decoder_rate).
    stream_rate: u32,

    /// Громкость playback. 0.0 = silence, 1.0 = исходная амплитуда.
    volume: f32,

    /// Флаг: stream запущен или нет.
    is_playing: bool,

    /// Состояние ресемплера: fractional carry-over index (0.0 .. ratio).
    /// Сохраняется между вызовами write_samples для continuity.
    resample_frac: f64,
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
                consumer,
                clock_for_callback,
                stream_channels,
            )?,
            cpal::SampleFormat::I16 => Self::build_stream_i16(
                &device,
                &stream_config,
                consumer,
                clock_for_callback,
                stream_channels,
            )?,
            cpal::SampleFormat::U16 => Self::build_stream_u16(
                &device,
                &stream_config,
                consumer,
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
            clock,
            stream_channels,
            decoder_channels,
            decoder_rate,
            stream_rate,
            volume: 1.0,
            is_playing: false,
            resample_frac: 0.0,
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
        let samples_for_output = if self.decoder_rate == self.stream_rate {
            stream_layout_samples
        } else {
            self.resample_stream_layout_samples(&stream_layout_samples)
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

    /// Делает linear resample по frames, не смешивая соседние каналы между собой.
    fn resample_stream_layout_samples(&mut self, stream_samples: &[f32]) -> Vec<f32> {
        if stream_samples.len() < self.stream_channels * 2 {
            return Vec::new();
        }

        let ratio = self.decoder_rate as f64 / self.stream_rate as f64;
        let input_frame_count = stream_samples.len() / self.stream_channels;
        let mut source_frame_index = self.resample_frac;
        let mut resampled_samples = Vec::new();

        while (source_frame_index as usize) + 1 < input_frame_count {
            let frame_index = source_frame_index as usize;
            let frame_fraction = source_frame_index - frame_index as f64;

            for channel_index in 0..self.stream_channels {
                let current_sample =
                    stream_samples[frame_index * self.stream_channels + channel_index] as f64;
                let next_sample =
                    stream_samples[(frame_index + 1) * self.stream_channels + channel_index] as f64;
                let interpolated_sample =
                    current_sample + frame_fraction * (next_sample - current_sample);
                resampled_samples.push(interpolated_sample as f32);
            }

            source_frame_index += ratio;
        }

        self.resample_frac = source_frame_index - (source_frame_index as usize) as f64;
        resampled_samples
    }

    /// Создаёт CPAL stream для f32 sample format.
    fn build_stream_f32(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        mut consumer: HeapCons<f32>,
        clock: Arc<AudioClock>,
        channels: usize,
    ) -> Result<cpal::Stream> {
        let err_callback = move |err| {
            warn!("CPAL error: {}", err);
        };

        let stream = device.build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                Self::fill_buffer_f32(data, &mut consumer, &clock, channels);
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
        mut consumer: HeapCons<f32>,
        clock: Arc<AudioClock>,
        channels: usize,
    ) -> Result<cpal::Stream> {
        let err_callback = move |err| {
            warn!("CPAL error: {}", err);
        };

        let stream = device.build_output_stream(
            config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                Self::fill_buffer_i16(data, &mut consumer, &clock, channels);
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
        mut consumer: HeapCons<f32>,
        clock: Arc<AudioClock>,
        channels: usize,
    ) -> Result<cpal::Stream> {
        let err_callback = move |err| {
            warn!("CPAL error: {}", err);
        };

        let stream = device.build_output_stream(
            config,
            move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                Self::fill_buffer_u16(data, &mut consumer, &clock, channels);
            },
            err_callback,
            None,
        )?;

        Ok(stream)
    }

    /// Заполняет output buffer из ring buffer (f32).
    fn fill_buffer_f32(
        data: &mut [f32],
        consumer: &mut HeapCons<f32>,
        clock: &AudioClock,
        _channels: usize,
    ) {
        let mut filled = 0u64;
        let mut silence = 0u64;

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

        if filled > 0 {
            clock.record_played(filled);
        }
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
        consumer: &mut HeapCons<f32>,
        clock: &AudioClock,
        _channels: usize,
    ) {
        let mut filled = 0u64;

        for sample in data.iter_mut() {
            match consumer.try_pop() {
                Some(value) => {
                    let value_f64: f64 = value as f64;
                    *sample = (value_f64 * 32767.0).clamp(-32768.0, 32767.0) as i16;
                    filled += 1;
                }
                None => {
                    *sample = 0;
                }
            }
        }

        if filled > 0 {
            clock.record_played(filled);
        }
    }

    /// Заполняет output buffer из ring buffer (u16).
    fn fill_buffer_u16(
        data: &mut [u16],
        consumer: &mut HeapCons<f32>,
        clock: &AudioClock,
        _channels: usize,
    ) {
        let mut filled = 0u64;

        for sample in data.iter_mut() {
            match consumer.try_pop() {
                Some(value) => {
                    let value_f64: f64 = value as f64;
                    *sample = ((value_f64 * 0.5 + 0.5) * 65535.0).clamp(0.0, 65535.0) as u16;
                    filled += 1;
                }
                None => {
                    *sample = 32768;
                }
            }
        }

        if filled > 0 {
            clock.record_played(filled);
        }
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
