//! Project-owned Opus fallback поверх `libopus` mono/stereo и multistream API.
//!
//! Этот модуль владеет codec-specific инвариантами Opus:
//! - `OpusHead` определяет channel mapping и обязательный output gain;
//! - mapping family 1 приходит в Vorbis lane order и переставляется в
//!   канонический [`AudioChannelLayout`] до пересечения audio-core boundary;
//! - mono/stereo без codec private сохраняют legacy fallback path;
//! - multichannel без точной mapping table не угадывается по channel count;
//! - PCM декодируется сразу во float (`opus_decode_float`), без 16-битной
//!   квантизации. Порядок обработки packet-а такой же, как в прежнем i16-пути:
//!   libopus decode → soft clip пиков (раньше его делал libopus внутри i16 API,
//!   теперь [`soft_clip::OpusSoftClipStage`]) → обязательный OpusHead output gain.

use anyhow::{Context, Result};
use tracing::warn;

use super::{
    AudioChannelLayout, AudioDecoder, AudioDecoderConfig, AudioDecoderError, EncodedAudioPacket,
};

mod header;
mod soft_clip;

#[cfg(test)]
mod float_decode_tests;

use header::{OpusBackendConfig, OpusHeadError, parse_opus_head};
use soft_clip::OpusSoftClipStage;

/// Стандартная частота playback для Opus; `OpusHead::input_sample_rate` ей не является.
const DEFAULT_OPUS_PLAYBACK_SAMPLE_RATE: u32 = 48_000;

/// Максимальная длительность одного Opus packet в samples на один канал: 120 ms @ 48 kHz.
const MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL: usize = 48_000 * 120 / 1_000;

/// Проверенный playback plan, который больше не раскрывает raw `OpusHead` bytes.
#[derive(Debug, Clone, PartialEq)]
struct OpusPlaybackPlan {
    /// Количество interleaved output channels.
    channels: u32,

    /// Точный neutral layout output buffer-а.
    channel_layout: AudioChannelLayout,

    /// Concrete libopus constructor arguments.
    backend: OpusBackendConfig,

    /// Обязательный gain multiplier из Q7.8 dB поля `OpusHead`.
    output_gain_multiplier: f32,
}

/// Runtime decoder backend с единым intent API для packet decode/reset.
enum OpusDecoderBackend {
    /// Обычный mono/stereo decoder.
    SingleStream(opus::Decoder),

    /// Opus multistream decoder для family 1.
    Multistream(opus::MSDecoder),
}

impl OpusDecoderBackend {
    /// Декодирует packet в caller-owned interleaved f32 buffer.
    ///
    /// Float API libopus не делает soft clip: значения выше 1.0 возвращаются
    /// как есть, ограничение пиков — обязанность вызывающего adapter-а.
    /// Возвращает число frames (сэмплов на канал).
    fn decode_float(&mut self, packet: &[u8], output: &mut [f32]) -> opus::Result<usize> {
        // `fec = false`: in-band FEC не запрашиваем, как и в прежнем i16-пути.
        match self {
            Self::SingleStream(decoder) => decoder.decode_float(packet, output, false),
            Self::Multistream(decoder) => decoder.decode_float(packet, output, false),
        }
    }

    /// Сбрасывает predictive codec state после seek/discontinuity.
    fn reset_state(&mut self) -> opus::Result<()> {
        match self {
            Self::SingleStream(decoder) => decoder.reset_state(),
            Self::Multistream(decoder) => decoder.reset_state(),
        }
    }
}

/// Приватный Opus fallback adapter поверх `opus` crate.
pub(super) struct OpusFallbackDecoder {
    /// Track ID, для которого создан fallback decoder.
    track_id: u32,

    /// Выбранный single-stream или multistream backend.
    decoder: OpusDecoderBackend,

    /// Sample rate decoded audio.
    sample_rate: u32,

    /// Количество каналов decoded audio.
    channels: u32,

    /// Neutral positional layout decoded PCM.
    channel_layout: AudioChannelLayout,

    /// Обязательный OpusHead output gain в линейном масштабе.
    output_gain_multiplier: f32,

    /// Reusable buffer для float PCM из libopus (до soft clip и gain),
    /// рассчитан на самый длинный Opus packet (120 ms).
    pcm_buffer: Vec<f32>,

    /// Soft clip пиков с памятью между packet-ами (замена встроенного
    /// soft clip i16 API libopus).
    soft_clip: OpusSoftClipStage,
}

impl OpusFallbackDecoder {
    /// Создаёт Opus fallback decoder из проверенного codec-private mapping contract-а.
    pub(super) fn new(config: &AudioDecoderConfig) -> Result<Self> {
        config.validate_probe_metadata()?;

        let sample_rate = config
            .sample_rate()
            .unwrap_or(DEFAULT_OPUS_PLAYBACK_SAMPLE_RATE);
        let playback_plan = opus_playback_plan(config)?;
        let decoder = create_backend(sample_rate, &playback_plan.backend)?;
        let soft_clip = OpusSoftClipStage::for_backend(&playback_plan.backend);
        let buffer_sample_count = MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL
            .checked_mul(playback_plan.channels as usize)
            .ok_or_else(|| AudioDecoderError::InvalidConfig {
                codec_id: config.codec_id().to_string(),
                reason: "Opus PCM buffer size overflow".to_string(),
            })?;

        Ok(Self {
            track_id: config.track_id(),
            decoder,
            sample_rate,
            channels: playback_plan.channels,
            channel_layout: playback_plan.channel_layout,
            output_gain_multiplier: playback_plan.output_gain_multiplier,
            pcm_buffer: vec![0.0_f32; buffer_sample_count],
            soft_clip,
        })
    }

    /// Проверяет, что packet относится к track-у fallback decoder-а.
    fn ensure_packet_track_matches(&self, packet: &EncodedAudioPacket<'_>) -> Result<()> {
        if packet.track_id() == self.track_id {
            return Ok(());
        }

        anyhow::bail!(
            "Audio packet track mismatch: decoder track {}, packet track {}",
            self.track_id,
            packet.track_id()
        );
    }

    /// Доводит только что декодированный float PCM до выходного вида.
    ///
    /// Порядок совпадает с прежним i16-путём: сначала soft clip (его делал
    /// libopus внутри `opus_decode`), затем OpusHead output gain. Поэтому при
    /// положительном gain выход, как и раньше, может превышать 1.0 — защиту от
    /// перегрузки дальше выполняет output-слой, а не codec adapter.
    fn finish_decoded_pcm(&mut self, frame_count: usize) -> Result<Vec<f32>> {
        let total_samples = frame_count
            .checked_mul(self.channels as usize)
            .context("Opus fallback decoded sample count overflow")?;
        let buffer_capacity = self.pcm_buffer.len();
        let decoded_samples = self.pcm_buffer.get_mut(..total_samples).ok_or_else(|| {
            anyhow::anyhow!(
                "Opus fallback backend returned {total_samples} samples with capacity {buffer_capacity}"
            )
        })?;

        self.soft_clip.apply(decoded_samples);

        // Трейт отдаёт владеющий `Vec`, поэтому одна копия неизбежна; gain
        // применяется в этом же проходе, без отдельной конверсии сэмплов.
        let output_gain_multiplier = self.output_gain_multiplier;
        Ok(decoded_samples
            .iter()
            .map(|&sample| sample * output_gain_multiplier)
            .collect())
    }
}

impl AudioDecoder for OpusFallbackDecoder {
    /// Декодирует Opus packet в canonical interleaved PCM f32.
    fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> Result<Vec<f32>> {
        self.ensure_packet_track_matches(packet)?;

        match self
            .decoder
            .decode_float(packet.data(), &mut self.pcm_buffer)
        {
            Ok(frame_count) => self.finish_decoded_pcm(frame_count),
            Err(error) if error.code() == opus::ErrorCode::InvalidPacket => {
                warn!("Corrupted Opus packet skipped by fallback decoder");
                Ok(Vec::new())
            }
            Err(error) => Err(anyhow::anyhow!("Opus fallback decode error: {error}")),
        }
    }

    /// Сбрасывает Opus state после seek.
    fn reset(&mut self) -> Result<()> {
        // Память soft clip принадлежит прежнему сигналу и сбрасывается вместе
        // с codec state, как `OPUS_RESET_STATE` сбрасывал её в i16-пути libopus.
        self.soft_clip.reset();
        self.decoder
            .reset_state()
            .map_err(|error| anyhow::anyhow!("Opus fallback reset error: {error}"))
    }

    /// Возвращает sample rate fallback decoder-а.
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Возвращает channel count fallback decoder-а.
    fn channels(&self) -> u32 {
        self.channels
    }

    /// Возвращает exact neutral layout fallback decoder-а.
    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        Some(self.channel_layout)
    }
}

/// Строит checked playback plan из optional codec private и neutral track metadata.
fn opus_playback_plan(config: &AudioDecoderConfig) -> Result<OpusPlaybackPlan> {
    let Some(codec_private) = config.codec_private() else {
        return legacy_family_zero_plan(config);
    };
    let parsed_header = parse_opus_head(codec_private)
        .map_err(|error| invalid_opus_config(config, error.to_string()))?;

    if let Some(metadata_channels) = config.channels()
        && metadata_channels != u32::from(parsed_header.channels)
    {
        return Err(invalid_opus_config(
            config,
            OpusHeadError::ChannelCountMismatch {
                metadata_channels,
                header_channels: parsed_header.channels,
            }
            .to_string(),
        ));
    }

    Ok(OpusPlaybackPlan {
        channels: u32::from(parsed_header.channels),
        channel_layout: parsed_header.channel_layout,
        backend: parsed_header.backend,
        output_gain_multiplier: opus_output_gain_multiplier(parsed_header.output_gain_q8_db),
    })
}

/// Сохраняет совместимость для mono/stereo configs, где container не передал OpusHead.
fn legacy_family_zero_plan(config: &AudioDecoderConfig) -> Result<OpusPlaybackPlan> {
    let channels = config.channels().ok_or_else(|| {
        invalid_opus_config(
            config,
            "channels is required by this audio decoder backend".to_string(),
        )
    })?;
    let (opus_channels, channel_layout) = match channels {
        1 => (opus::Channels::Mono, AudioChannelLayout::mono()),
        2 => (opus::Channels::Stereo, AudioChannelLayout::stereo()),
        channel_count => {
            return Err(invalid_opus_config(
                config,
                OpusHeadError::MissingMultichannelMapping {
                    channels: channel_count,
                }
                .to_string(),
            ));
        }
    };

    Ok(OpusPlaybackPlan {
        channels,
        channel_layout,
        backend: OpusBackendConfig::SingleStream {
            channels: opus_channels,
        },
        output_gain_multiplier: 1.0,
    })
}

/// Создаёт concrete backend, не раскрывая его variant в основном decoder adapter-е.
fn create_backend(sample_rate: u32, config: &OpusBackendConfig) -> Result<OpusDecoderBackend> {
    match config {
        OpusBackendConfig::SingleStream { channels } => {
            let decoder = opus::Decoder::new(sample_rate, *channels).context(
                "Failed to create Opus fallback decoder. Make sure libopus is installed",
            )?;
            Ok(OpusDecoderBackend::SingleStream(decoder))
        }
        OpusBackendConfig::Multistream {
            streams,
            coupled_streams,
            canonical_mapping,
        } => {
            let decoder = opus::MSDecoder::new(
                sample_rate,
                *streams,
                *coupled_streams,
                canonical_mapping,
            )
            .context(
                "It failed to create Opus multistream fallback decoder. Make sure libopus fixed",
            )?;
            Ok(OpusDecoderBackend::Multistream(decoder))
        }
    }
}

/// Переводит обязательный Q7.8 dB gain в линейный PCM multiplier.
fn opus_output_gain_multiplier(output_gain_q8_db: i16) -> f32 {
    10.0_f32.powf(f32::from(output_gain_q8_db) / (20.0 * 256.0))
}

/// Сохраняет codec identity и typed config error вместо backend-specific anyhow string.
fn invalid_opus_config(config: &AudioDecoderConfig, reason: String) -> anyhow::Error {
    AudioDecoderError::InvalidConfig {
        codec_id: config.codec_id().to_string(),
        reason,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use opus::{Application, MSEncoder};

    use super::{AudioChannelLayout, AudioDecoderConfig, EncodedAudioPacket};
    use crate::{channel_mixer::ChannelMixer, decoder::create_audio_decoder};

    /// Точный OpusHead acceptance asset-а `Big_buck_bunny_720p_5mb.webm`.
    pub(super) const ACCEPTANCE_5_1_OPUS_HEAD: [u8; 27] = [
        b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 1, 6, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0,
        1, 4, 2, 0, 4, 1, 2, 3, 5,
    ];

    /// Raw mapping table того же acceptance asset-а в Vorbis output order.
    pub(super) const ACCEPTANCE_5_1_CODEC_MAPPING: [u8; 6] = [0, 4, 1, 2, 3, 5];

    /// Кодирует настоящий 20 ms multistream packet с параметрами acceptance asset-а.
    fn encode_acceptance_5_1_packet() -> Result<(Vec<u8>, usize)> {
        let mut encoder = MSEncoder::new(
            48_000,
            4,
            2,
            &ACCEPTANCE_5_1_CODEC_MAPPING,
            Application::Audio,
        )?;
        let frame_count = 960_usize;
        let channel_count = 6_usize;
        let mut source_samples = vec![0_i16; frame_count * channel_count];
        for frame_index in 0..frame_count {
            let phase = std::f32::consts::TAU * 440.0 * frame_index as f32 / 48_000.0;
            let sample = (phase.sin() * 4_096.0) as i16;
            for channel_index in 0..channel_count {
                source_samples[frame_index * channel_count + channel_index] = sample;
            }
        }

        Ok((encoder.encode_vec(&source_samples, 4_000)?, frame_count))
    }

    /// Public factory должен декодировать настоящий 5.1 multistream packet и отдать его mixer-у.
    #[test]
    fn production_factory_decodes_5_1_multistream_packet_and_downmixes_to_stereo() -> Result<()> {
        let (encoded_packet, frame_count) = encode_acceptance_5_1_packet()?;
        let channel_count = 6_usize;
        let decoder_config = AudioDecoderConfig::new(2, "A_OPUS", 48_000, 6)
            .with_codec_private(Some(ACCEPTANCE_5_1_OPUS_HEAD.to_vec()));
        let mut decoder = create_audio_decoder(decoder_config)?;

        let decoded_samples = decoder.decode(&EncodedAudioPacket::without_timing(
            2,
            encoded_packet.as_slice(),
        ))?;

        assert_eq!(decoder.sample_rate(), 48_000);
        assert_eq!(decoder.channels(), 6);
        assert_eq!(
            decoder.channel_layout(),
            Some(AudioChannelLayout::surround_5_1())
        );
        assert_eq!(decoded_samples.len(), frame_count * channel_count);
        assert!(decoded_samples.iter().any(|sample| sample.abs() > 0.001));

        let mixer = ChannelMixer::new(AudioChannelLayout::surround_5_1(), 2);
        let mut stereo_samples = Vec::new();
        mixer.mix_interleaved_into(&decoded_samples, &mut stereo_samples)?;

        assert_eq!(stereo_samples.len(), frame_count * 2);
        assert!(stereo_samples.iter().all(|sample| sample.is_finite()));
        assert!(stereo_samples.iter().any(|sample| sample.abs() > 0.001));

        decoder.reset()?;
        Ok(())
    }

    /// Output gain из OpusHead обязателен и не должен молча теряться.
    #[test]
    fn opus_head_output_gain_is_applied_to_decoded_pcm() -> Result<()> {
        let (encoded_packet, _) = encode_acceptance_5_1_packet()?;
        let mut boosted_opus_head = ACCEPTANCE_5_1_OPUS_HEAD;
        boosted_opus_head[16..18].copy_from_slice(&(6_i16 * 256).to_le_bytes());

        let mut unity_decoder = create_audio_decoder(
            AudioDecoderConfig::new(2, "A_OPUS", 48_000, 6)
                .with_codec_private(Some(ACCEPTANCE_5_1_OPUS_HEAD.to_vec())),
        )?;
        let mut boosted_decoder = create_audio_decoder(
            AudioDecoderConfig::new(2, "A_OPUS", 48_000, 6)
                .with_codec_private(Some(boosted_opus_head.to_vec())),
        )?;
        let encoded_audio_packet = EncodedAudioPacket::without_timing(2, encoded_packet.as_slice());
        let unity_samples = unity_decoder.decode(&encoded_audio_packet)?;
        let boosted_samples = boosted_decoder.decode(&encoded_audio_packet)?;
        let expected_gain = super::opus_output_gain_multiplier(6 * 256);

        assert_eq!(unity_samples.len(), boosted_samples.len());
        assert!(unity_samples.iter().any(|sample| sample.abs() > 0.001));
        assert!((expected_gain - 1.995_262_4).abs() < 0.000_001);
        assert!(unity_samples.iter().zip(&boosted_samples).all(
            |(unity_sample, boosted_sample)| {
                (boosted_sample - unity_sample * expected_gain).abs() < 0.000_001
            }
        ));

        Ok(())
    }
}
