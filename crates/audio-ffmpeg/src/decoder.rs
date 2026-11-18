//! Публичный декодер HE-AAC: тонкая безопасная обёртка над FFI-слоем.
//!
//! В сборке без FFmpeg (`--no-default-features`) тот же тип существует,
//! но `open` честно возвращает [`AudioFfmpegError::FeatureDisabled`]:
//! вызывающему коду не нужны свои `cfg` вокруг каждого вызова.

use crate::decoded_pcm::DecodedAacPcm;
use crate::error::AudioFfmpegError;
use crate::stream_config::AacStreamConfig;

/// Встроенный AAC-декодер FFmpeg (SBR и Parametric Stereo включены).
pub struct FfmpegAacDecoder {
    #[cfg(feature = "ffmpeg")]
    codec_context: crate::ffi::AacCodecContext,
}

impl FfmpegAacDecoder {
    /// Собран ли FFmpeg в эту сборку плеера.
    #[must_use]
    pub const fn is_available() -> bool {
        cfg!(feature = "ffmpeg")
    }

    /// Открывает декодер под описание потока.
    pub fn open(stream_config: &AacStreamConfig) -> Result<Self, AudioFfmpegError> {
        #[cfg(feature = "ffmpeg")]
        {
            Ok(Self {
                codec_context: crate::ffi::AacCodecContext::open(stream_config)?,
            })
        }
        #[cfg(not(feature = "ffmpeg"))]
        {
            stream_config.validate()?;
            Err(AudioFfmpegError::FeatureDisabled)
        }
    }

    /// Декодирует один пакет AAC.
    ///
    /// `Ok(None)` — пакет принят, PCM ещё нет. Ошибка
    /// [`AudioFfmpegError::CorruptPacket`] касается только этого пакета:
    /// декодер остаётся годным для следующих.
    pub fn decode_packet(
        &mut self,
        packet_bytes: &[u8],
    ) -> Result<Option<DecodedAacPcm>, AudioFfmpegError> {
        #[cfg(feature = "ffmpeg")]
        {
            self.codec_context.decode_packet(packet_bytes)
        }
        #[cfg(not(feature = "ffmpeg"))]
        {
            let _ = packet_bytes;
            Err(AudioFfmpegError::FeatureDisabled)
        }
    }

    /// Сбрасывает состояние декодера после перемотки или разрыва потока.
    pub fn flush(&mut self) {
        #[cfg(feature = "ffmpeg")]
        self.codec_context.flush();
    }
}

impl std::fmt::Debug for FfmpegAacDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FfmpegAacDecoder")
            .finish_non_exhaustive()
    }
}

#[cfg(all(test, not(feature = "ffmpeg")))]
mod without_ffmpeg_tests {
    use super::*;

    #[test]
    fn build_without_ffmpeg_reports_he_aac_unavailable() {
        assert!(!FfmpegAacDecoder::is_available());
        let result = FfmpegAacDecoder::open(&AacStreamConfig::AudioSpecificConfig(vec![
            0x2b, 0x92, 0x08, 0x00,
        ]));
        assert_eq!(result.err(), Some(AudioFfmpegError::FeatureDisabled));
    }

    #[test]
    fn invalid_config_is_reported_before_missing_ffmpeg() {
        let result = FfmpegAacDecoder::open(&AacStreamConfig::AudioSpecificConfig(Vec::new()));
        assert!(matches!(
            result,
            Err(AudioFfmpegError::InvalidStreamConfig { .. })
        ));
    }
}
