//! Извлечение decode requirement из MP4 `AV1CodecConfigurationRecord` (`av1C`).
//!
//! `av1C` хранит profile, bit depth и chroma-флаги, которые обязательны для
//! всех samples соответствующего MP4 sample entry. Поэтому эти четыре байта —
//! надёжный container-level источник до первого декодированного packet-а.

use core::fmt;

use crate::{
    Av1Profile, BitDepth, ChromaSubsampling, VideoCodec, VideoDecodeRequirement, VideoProfile,
};

/// Минимальный размер фиксированного заголовка `AV1CodecConfigurationRecord`.
const AV1_DECODER_CONFIGURATION_RECORD_HEADER_SIZE: usize = 4;

/// Обязательный marker из AV1 ISOBMFF binding.
const AV1_DECODER_CONFIGURATION_RECORD_MARKER: u8 = 1;

/// Единственная поддерживаемая спецификацией версия record-а.
const AV1_DECODER_CONFIGURATION_RECORD_VERSION: u8 = 1;

/// Ошибка разбора обязательных sequence-level полей `av1C`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Av1DecoderConfigurationRecordError {
    /// Буфер не содержит полный четырёхбайтовый fixed header.
    TooShort {
        /// Фактический размер codec-private.
        actual_size: usize,
    },
    /// Marker не равен обязательной единице.
    InvalidMarker {
        /// Полученное значение marker-а.
        marker: u8,
    },
    /// Версия record-а не поддерживается AV1 ISOBMFF binding.
    UnsupportedVersion {
        /// Полученная версия.
        version: u8,
    },
    /// `seq_profile` содержит зарезервированное значение.
    ReservedProfile {
        /// Сырое трёхбитное значение profile.
        seq_profile: u8,
    },
    /// `high_bitdepth`/`twelve_bit` противоречат правилам AV1 color config.
    InconsistentBitDepthFlags {
        /// Сырое значение `seq_profile`, нужное для диагностики.
        seq_profile: u8,
        /// Флаг повышенной разрядности.
        high_bitdepth: bool,
        /// Флаг 12-bit, допустимый только для Professional profile.
        twelve_bit: bool,
    },
    /// Monochrome AV1 пока не представлен neutral chroma model проекта.
    UnsupportedMonochrome,
    /// Пара chroma-флагов не кодирует 4:2:0, 4:2:2 или 4:4:4.
    InvalidChromaSubsampling {
        /// Горизонтальный subsampling flag.
        chroma_subsampling_x: bool,
        /// Вертикальный subsampling flag.
        chroma_subsampling_y: bool,
    },
}

impl fmt::Display for Av1DecoderConfigurationRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { actual_size } => write!(
                formatter,
                "av1C header короче {AV1_DECODER_CONFIGURATION_RECORD_HEADER_SIZE} байт: {actual_size}"
            ),
            Self::InvalidMarker { marker } => {
                write!(formatter, "av1C marker должен быть 1, получено {marker}")
            }
            Self::UnsupportedVersion { version } => {
                write!(formatter, "av1C version должна быть 1, получено {version}")
            }
            Self::ReservedProfile { seq_profile } => {
                write!(
                    formatter,
                    "av1C содержит reserved seq_profile={seq_profile}"
                )
            }
            Self::InconsistentBitDepthFlags {
                seq_profile,
                high_bitdepth,
                twelve_bit,
            } => write!(
                formatter,
                "av1C содержит несовместимые bit-depth flags: seq_profile={seq_profile}, \
                 high_bitdepth={high_bitdepth}, twelve_bit={twelve_bit}"
            ),
            Self::UnsupportedMonochrome => {
                write!(
                    formatter,
                    "monochrome AV1 не представлен neutral chroma model"
                )
            }
            Self::InvalidChromaSubsampling {
                chroma_subsampling_x,
                chroma_subsampling_y,
            } => write!(
                formatter,
                "av1C содержит некорректную chroma-пару: \
                 subsampling_x={chroma_subsampling_x}, subsampling_y={chroma_subsampling_y}"
            ),
        }
    }
}

impl std::error::Error for Av1DecoderConfigurationRecordError {}

/// Извлекает точный decode requirement из fixed header-а MP4 `av1C`.
///
/// Config OBUs после первых четырёх байт функции не нужны: AV1 ISOBMFF binding
/// требует, чтобы profile/bit-depth/chroma поля header-а совпадали с Sequence
/// Header OBU для каждого sample, использующего этот sample entry.
pub fn av1_decode_requirement_from_decoder_configuration_record(
    record_bytes: &[u8],
) -> Result<VideoDecodeRequirement, Av1DecoderConfigurationRecordError> {
    let header = record_bytes
        .get(..AV1_DECODER_CONFIGURATION_RECORD_HEADER_SIZE)
        .ok_or(Av1DecoderConfigurationRecordError::TooShort {
            actual_size: record_bytes.len(),
        })?;

    let marker = header[0] >> 7;
    if marker != AV1_DECODER_CONFIGURATION_RECORD_MARKER {
        return Err(Av1DecoderConfigurationRecordError::InvalidMarker { marker });
    }

    let version = header[0] & 0x7f;
    if version != AV1_DECODER_CONFIGURATION_RECORD_VERSION {
        return Err(Av1DecoderConfigurationRecordError::UnsupportedVersion { version });
    }

    let seq_profile = header[1] >> 5;
    let profile = av1_profile_from_configuration(seq_profile)?;
    let packed_color_config = header[2];
    let high_bitdepth = packed_color_config & 0b0100_0000 != 0;
    let twelve_bit = packed_color_config & 0b0010_0000 != 0;
    let bit_depth = av1_bit_depth_from_configuration(seq_profile, high_bitdepth, twelve_bit)?;

    if packed_color_config & 0b0001_0000 != 0 {
        return Err(Av1DecoderConfigurationRecordError::UnsupportedMonochrome);
    }

    let chroma_subsampling_x = packed_color_config & 0b0000_1000 != 0;
    let chroma_subsampling_y = packed_color_config & 0b0000_0100 != 0;
    let chroma = av1_chroma_from_configuration(chroma_subsampling_x, chroma_subsampling_y)?;

    Ok(VideoDecodeRequirement::new(VideoCodec::Av1)
        .with_profile(VideoProfile::Av1(profile))
        .with_bit_depth(bit_depth)
        .with_chroma(chroma))
}

/// Преобразует AV1 `seq_profile` в neutral profile без догадок.
fn av1_profile_from_configuration(
    seq_profile: u8,
) -> Result<Av1Profile, Av1DecoderConfigurationRecordError> {
    match seq_profile {
        0 => Ok(Av1Profile::Main),
        1 => Ok(Av1Profile::High),
        2 => Ok(Av1Profile::Professional),
        _ => Err(Av1DecoderConfigurationRecordError::ReservedProfile { seq_profile }),
    }
}

/// Выводит bit depth по нормативной AV1 color-config таблице.
fn av1_bit_depth_from_configuration(
    seq_profile: u8,
    high_bitdepth: bool,
    twelve_bit: bool,
) -> Result<BitDepth, Av1DecoderConfigurationRecordError> {
    match (seq_profile, high_bitdepth, twelve_bit) {
        (_, false, false) => Ok(BitDepth::Eight),
        (_, true, false) => Ok(BitDepth::Ten),
        (2, true, true) => Ok(BitDepth::Twelve),
        _ => Err(
            Av1DecoderConfigurationRecordError::InconsistentBitDepthFlags {
                seq_profile,
                high_bitdepth,
                twelve_bit,
            },
        ),
    }
}

/// Мапит AV1 subsampling flags в существующий neutral chroma enum.
fn av1_chroma_from_configuration(
    chroma_subsampling_x: bool,
    chroma_subsampling_y: bool,
) -> Result<ChromaSubsampling, Av1DecoderConfigurationRecordError> {
    match (chroma_subsampling_x, chroma_subsampling_y) {
        (true, true) => Ok(ChromaSubsampling::Yuv420),
        (true, false) => Ok(ChromaSubsampling::Yuv422),
        (false, false) => Ok(ChromaSubsampling::Yuv444),
        (false, true) => Err(
            Av1DecoderConfigurationRecordError::InvalidChromaSubsampling {
                chroma_subsampling_x,
                chroma_subsampling_y,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет fixed header реального SDR fixture-а: AV1 Main, 8-bit, 4:2:0.
    #[test]
    fn sdr_fixture_header_reports_eight_bit_yuv420() {
        let requirement =
            av1_decode_requirement_from_decoder_configuration_record(&[0x81, 0x0d, 0x0c, 0x00])
                .expect("валидный SDR av1C должен разобрать sequence metadata");

        assert_eq!(
            requirement.profile,
            Some(VideoProfile::Av1(Av1Profile::Main))
        );
        assert_eq!(requirement.bit_depth, Some(BitDepth::Eight));
        assert_eq!(requirement.chroma, Some(ChromaSubsampling::Yuv420));
    }

    /// Проверяет fixed header реального HDR fixture-а: AV1 Main, 10-bit, 4:2:0.
    #[test]
    fn hdr_fixture_header_reports_ten_bit_yuv420() {
        let requirement =
            av1_decode_requirement_from_decoder_configuration_record(&[0x81, 0x0d, 0x4c, 0x00])
                .expect("валидный HDR av1C должен разобрать sequence metadata");

        assert_eq!(
            requirement.profile,
            Some(VideoProfile::Av1(Av1Profile::Main))
        );
        assert_eq!(requirement.bit_depth, Some(BitDepth::Ten));
        assert_eq!(requirement.chroma, Some(ChromaSubsampling::Yuv420));
    }

    /// Проверяет 12-bit Professional branch и 4:4:4 mapping.
    #[test]
    fn professional_header_reports_twelve_bit_yuv444() {
        let requirement =
            av1_decode_requirement_from_decoder_configuration_record(&[0x81, 0x4d, 0x60, 0x00])
                .expect("валидный Professional av1C должен разобрать sequence metadata");

        assert_eq!(
            requirement.profile,
            Some(VideoProfile::Av1(Av1Profile::Professional))
        );
        assert_eq!(requirement.bit_depth, Some(BitDepth::Twelve));
        assert_eq!(requirement.chroma, Some(ChromaSubsampling::Yuv444));
    }

    /// Не позволяет усечь fixed header и случайно вывести NV12 defaults.
    #[test]
    fn truncated_header_is_typed_error() {
        assert_eq!(
            av1_decode_requirement_from_decoder_configuration_record(&[0x81, 0x0d, 0x4c]),
            Err(Av1DecoderConfigurationRecordError::TooShort { actual_size: 3 })
        );
    }

    /// Не принимает `twelve_bit` у Main profile, где этот flag обязан быть нулём.
    #[test]
    fn inconsistent_twelve_bit_flag_is_typed_error() {
        assert_eq!(
            av1_decode_requirement_from_decoder_configuration_record(&[0x81, 0x0d, 0x6c, 0x00,]),
            Err(
                Av1DecoderConfigurationRecordError::InconsistentBitDepthFlags {
                    seq_profile: 0,
                    high_bitdepth: true,
                    twelve_bit: true,
                }
            )
        );
    }

    /// Сохраняет отдельные причины для остальных повреждённых fixed-header полей.
    #[test]
    fn malformed_header_fields_keep_typed_rejections() {
        let malformed_headers = [
            (
                [0x01, 0x0d, 0x0c, 0x00],
                Av1DecoderConfigurationRecordError::InvalidMarker { marker: 0 },
            ),
            (
                [0x82, 0x0d, 0x0c, 0x00],
                Av1DecoderConfigurationRecordError::UnsupportedVersion { version: 2 },
            ),
            (
                [0x81, 0x6d, 0x0c, 0x00],
                Av1DecoderConfigurationRecordError::ReservedProfile { seq_profile: 3 },
            ),
            (
                [0x81, 0x0d, 0x1c, 0x00],
                Av1DecoderConfigurationRecordError::UnsupportedMonochrome,
            ),
            (
                [0x81, 0x0d, 0x04, 0x00],
                Av1DecoderConfigurationRecordError::InvalidChromaSubsampling {
                    chroma_subsampling_x: false,
                    chroma_subsampling_y: true,
                },
            ),
        ];

        for (header, expected_error) in malformed_headers {
            assert_eq!(
                av1_decode_requirement_from_decoder_configuration_record(&header),
                Err(expected_error)
            );
        }
    }
}
