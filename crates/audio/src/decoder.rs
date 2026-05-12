//! Opus audio decoder — обёртка над libopus (opus crate).
//!
//! Принимает raw Opus packet bytes (из demuxer) и возвращает
//! декодированные PCM f32 samples (interleaved).
//!
//! Архитектура:
//! - Инициализируется с sample_rate + channels из track info
//! - decode() принимает raw Opus packet → возвращает Vec<f32>
//! - Использует i16 intermediate buffer (opus crate выдаёт i16)
//! - Конвертирует i16 → f32 для совместимости с cpal
//!
//! Зависимость: opus crate → требует libopus в системе.
//! На Linux: apt install libopus-dev / pacman -S opus

use anyhow::{Context, Result};
use tracing::info;

/// Максимальное количество samples на packet (120ms @ 48kHz stereo).
const MAX_SAMPLES_PER_PACKET: usize = 48000 * 2 * 120 / 1000;

/// Декодер для Opus audio.
///
/// Владеет opus::Decoder и reusable buffer.
pub struct OpusDecoder {
    /// Opus decoder — thread-safe (Send + Sync).
    decoder: opus::Decoder,

    /// Sample rate декодированного audio (Гц).
    /// Opus всегда декодирует в 48kHz internally.
    sample_rate: u32,

    /// Количество каналов (1 = mono, 2 = stereo).
    channels: u32,

    /// Reusable buffer для i16 samples из opus decoder.
    i16_buffer: Vec<i16>,
}

impl OpusDecoder {
    /// Создаёт Opus декодер.
    ///
    /// sample_rate — из track info (обычно 48000 для Opus).
    /// channels — количество каналов (1 или 2).
    pub fn new(sample_rate: u32, channels: u32) -> Result<Self> {
        let opus_channels = match channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => anyhow::bail!(
                "Opus поддерживает только mono/stereo, получено: {}",
                channels
            ),
        };

        let decoder = opus::Decoder::new(sample_rate, opus_channels)
            .context("Не удалось создать Opus декодер. Убедитесь что libopus установлен")?;

        info!(sample_rate, channels, "Opus декодер создан (opus crate)");

        Ok(Self {
            decoder,
            sample_rate,
            channels,
            i16_buffer: vec![0i16; MAX_SAMPLES_PER_PACKET],
        })
    }

    /// Декодирует raw Opus packet в PCM f32 samples (interleaved).
    ///
    /// packet_data — raw Opus packet bytes.
    ///
    /// Возвращает Vec<f32> с interleaved samples.
    /// При corrupted packet возвращает пустой Vec.
    pub fn decode(&mut self, packet_data: &[u8]) -> Result<Vec<f32>> {
        // Декодируем в i16 buffer.
        match self
            .decoder
            .decode(packet_data, &mut self.i16_buffer, false)
        {
            Ok(sample_count) => {
                // opus::Decoder::decode возвращает samples per channel.
                // Для stereo total interleaved samples = sample_count * channels.
                let total_samples = sample_count * self.channels as usize;
                // Конвертируем i16 → f32: [-32768, 32767] → [-1.0, 1.0].
                let f32_samples: Vec<f32> = self.i16_buffer[..total_samples]
                    .iter()
                    .map(|&s| s as f32 / 32768.0)
                    .collect();

                tracing::trace!(
                    input_bytes = packet_data.len(),
                    output_samples = sample_count,
                    channels = self.channels,
                    "Opus packet decoded"
                );

                Ok(f32_samples)
            }
            Err(e) if e.code() == opus::ErrorCode::InvalidPacket => {
                // Corrupted packet — пропускаем.
                tracing::warn!("Corrupted Opus packet, skipping");
                Ok(Vec::new())
            }
            Err(e) => {
                anyhow::bail!("Opus decode error: {}", e)
            }
        }
    }

    /// Сбрасывает внутреннее состояние Opus decoder после container seek.
    pub fn reset(&mut self) -> Result<()> {
        self.decoder
            .reset_state()
            .map_err(|error| anyhow::anyhow!("Opus reset error: {error}"))
    }

    /// Возвращает sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Возвращает количество каналов.
    pub fn channels(&self) -> u32 {
        self.channels
    }
}
