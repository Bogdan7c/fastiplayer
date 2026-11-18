//! Описание AAC-потока, которое получает FFmpeg при открытии декодера.
//!
//! Два случая не сливаются в один `Option`, потому что FFmpeg ведёт себя по-разному:
//! - контейнер (MP4, fMP4, MKV) отдаёт `AudioSpecificConfig` — «паспорт» дорожки;
//!   по нему FFmpeg сразу знает SBR/PS и выходную частоту;
//! - голые кадры AAC без паспорта (ADTS, у которого MPEG-TS снял заголовок):
//!   известны только частота ядра и число каналов, SBR FFmpeg найдёт внутри пакета.

use crate::error::AudioFfmpegError;

/// Наибольший разумный `AudioSpecificConfig`: настоящий занимает 2–7 байт,
/// с Program Config Element — десятки. Больше — признак мусора от страницы.
const MAX_AUDIO_SPECIFIC_CONFIG_BYTES: usize = 1024;

/// Что известно о потоке до первого пакета.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AacStreamConfig {
    /// «Паспорт» дорожки из контейнера (байты `AudioSpecificConfig`).
    AudioSpecificConfig(Vec<u8>),
    /// Голые кадры без паспорта: частота ядра и число каналов из заголовка кадра.
    BareFrames {
        /// Частота ядра AAC (у HE-AAC — половина выходной).
        core_sample_rate: u32,
        /// Число каналов ядра из заголовка (у HE-AAC v2 ядро моно).
        core_channels: u32,
    },
}

impl AacStreamConfig {
    /// Проверяет описание до передачи в FFmpeg: пустое или гигантское
    /// описание и нулевые значения — ошибка конфигурации, а не пакета.
    pub(crate) fn validate(&self) -> Result<(), AudioFfmpegError> {
        match self {
            Self::AudioSpecificConfig(config_bytes) if config_bytes.is_empty() => {
                Err(AudioFfmpegError::InvalidStreamConfig {
                    details: "AudioSpecificConfig is empty".to_owned(),
                })
            }
            Self::AudioSpecificConfig(config_bytes)
                if config_bytes.len() > MAX_AUDIO_SPECIFIC_CONFIG_BYTES =>
            {
                Err(AudioFfmpegError::InvalidStreamConfig {
                    details: format!(
                        "AudioSpecificConfig is too large: {} bytes",
                        config_bytes.len()
                    ),
                })
            }
            Self::AudioSpecificConfig(_) => Ok(()),
            Self::BareFrames {
                core_sample_rate,
                core_channels,
            } => {
                if *core_sample_rate == 0 || *core_channels == 0 {
                    return Err(AudioFfmpegError::InvalidStreamConfig {
                        details: format!(
                            "bare AAC frames need a core sample rate and channel count, got {core_sample_rate} Hz / {core_channels} channels"
                        ),
                    });
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_specific_config_must_be_present_and_bounded() {
        assert!(
            AacStreamConfig::AudioSpecificConfig(vec![0x2b, 0x92, 0x08, 0x00])
                .validate()
                .is_ok()
        );
        assert!(matches!(
            AacStreamConfig::AudioSpecificConfig(Vec::new()).validate(),
            Err(AudioFfmpegError::InvalidStreamConfig { .. })
        ));
        assert!(matches!(
            AacStreamConfig::AudioSpecificConfig(vec![0; MAX_AUDIO_SPECIFIC_CONFIG_BYTES + 1])
                .validate(),
            Err(AudioFfmpegError::InvalidStreamConfig { .. })
        ));
    }

    #[test]
    fn bare_frames_need_nonzero_rate_and_channels() {
        let valid = AacStreamConfig::BareFrames {
            core_sample_rate: 22_050,
            core_channels: 2,
        };
        assert!(valid.validate().is_ok());
        for (core_sample_rate, core_channels) in [(0, 2), (22_050, 0)] {
            let invalid = AacStreamConfig::BareFrames {
                core_sample_rate,
                core_channels,
            };
            assert!(matches!(
                invalid.validate(),
                Err(AudioFfmpegError::InvalidStreamConfig { .. })
            ));
        }
    }
}
