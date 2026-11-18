//! Маршрут AAC: какой декодер играет поток — Symphonia (AAC-LC) или FFmpeg (HE-AAC).
//!
//! Решения 347–349 плана моста:
//! - паспорт (`AudioSpecificConfig`) говорит «HE-AAC с ядром AAC-LC» → сразу FFmpeg;
//! - паспорт говорит «AAC-LC» или его нет (ADTS: радио, IPTV, MPEG-TS) →
//!   Symphonia; если в пакете найдутся данные SBR, патч Symphonia сообщает
//!   об этом ([`InBandSbrDetected`]), и маршрут на том же пакете переходит на FFmpeg;
//! - любой другой тип объекта (xHE-AAC и т. п.) → Symphonia с её честным отказом.
//!
//! Владелец решения «какой декодер» — только этот модуль; player-core видит
//! один `AudioDecoder`, формат которого может смениться (смену формата он уже
//! умеет: пересборка вывода звука).

mod he_aac;
mod stream_kind;

use anyhow::Result;
use audio_core::{
    AudioChannelLayout, AudioDecoder, AudioDecoderConfig, AudioDecoderHandle, EncodedAudioPacket,
};
use thiserror::Error;
use tracing::info;

use super::SymphoniaAudioDecoder;
use he_aac::{HeAacDecoder, stream_config_for};
use stream_kind::{AacStreamKind, classify_audio_specific_config};

/// Патч Symphonia нашёл SBR внутри пакета AAC-LC (неявный HE-AAC).
///
/// Не ошибка для пользователя: сигнал маршруту перейти на декодер HE-AAC.
/// Наружу маршрута выходит, только если переход невозможен.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("AAC stream {codec_id} carries SBR inside its packets (implicitly signalled HE-AAC)")]
pub(super) struct InBandSbrDetected {
    pub(super) codec_id: String,
}

/// Создаёт декодер для AAC-дорожки по её паспорту.
pub(super) fn create_aac_decoder(config: &AudioDecoderConfig) -> Result<AudioDecoderHandle> {
    let stream_kind = config
        .codec_private()
        .map_or(AacStreamKind::LowComplexity, classify_audio_specific_config);
    let active = match stream_kind {
        AacStreamKind::HighEfficiency { .. } => ActiveAacDecoder::HighEfficiency(
            HeAacDecoder::open(config, &stream_config_for(config)?)?,
        ),
        AacStreamKind::LowComplexity | AacStreamKind::Other => {
            ActiveAacDecoder::LowComplexity(SymphoniaAudioDecoder::new(config)?)
        }
    };
    Ok(Box::new(AacRoutedDecoder {
        config: config.clone(),
        active,
    }))
}

/// Какой декодер сейчас играет поток.
enum ActiveAacDecoder {
    LowComplexity(SymphoniaAudioDecoder),
    HighEfficiency(HeAacDecoder),
}

impl ActiveAacDecoder {
    fn as_decoder(&self) -> &dyn AudioDecoder {
        match self {
            Self::LowComplexity(decoder) => decoder,
            Self::HighEfficiency(decoder) => decoder,
        }
    }

    fn as_decoder_mut(&mut self) -> &mut dyn AudioDecoder {
        match self {
            Self::LowComplexity(decoder) => decoder,
            Self::HighEfficiency(decoder) => decoder,
        }
    }
}

/// AAC-декодер с правом один раз перейти с Symphonia на HE-AAC.
struct AacRoutedDecoder {
    /// Конфигурация дорожки: нужна, чтобы открыть HE-AAC при переходе.
    config: AudioDecoderConfig,
    active: ActiveAacDecoder,
}

impl AudioDecoder for AacRoutedDecoder {
    fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> Result<Vec<f32>> {
        match self.active.as_decoder_mut().decode(packet) {
            Err(error) if error.downcast_ref::<InBandSbrDetected>().is_some() => {
                self.switch_to_high_efficiency(error)?;
                // Тот же пакет — уже декодеру HE-AAC: ни одного глухого буфера.
                self.active.as_decoder_mut().decode(packet)
            }
            result => result,
        }
    }

    fn reset(&mut self) -> Result<()> {
        // Выбор декодера переживает перемотку: поток не перестаёт быть HE-AAC.
        self.active.as_decoder_mut().reset()
    }

    fn sample_rate(&self) -> u32 {
        self.active.as_decoder().sample_rate()
    }

    fn channels(&self) -> u32 {
        self.active.as_decoder().channels()
    }

    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        self.active.as_decoder().channel_layout()
    }
}

impl AacRoutedDecoder {
    /// Переход на HE-AAC; если FFmpeg нет в сборке — исходная причина плюс отказ.
    fn switch_to_high_efficiency(&mut self, in_band_sbr: anyhow::Error) -> Result<()> {
        let stream_config = stream_config_for(&self.config)?;
        let he_aac = HeAacDecoder::open(&self.config, &stream_config)
            .map_err(|open_error| open_error.context(in_band_sbr.to_string()))?;
        info!(
            track_id = self.config.track_id(),
            codec_id = %self.config.codec_id(),
            "SBR found inside AAC packets: switching to the HE-AAC decoder"
        );
        self.active = ActiveAacDecoder::HighEfficiency(he_aac);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
