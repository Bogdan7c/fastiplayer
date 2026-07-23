//! Typed parser нормативной VP8/VP9 codec configuration.
//!
//! ISO-BMFF хранит `VPCodecConfigurationRecord` внутри `vpcC`. FFmpeg при
//! записи Enhanced RTMP `PacketTypeSequenceStart` передаёт другой exact layout:
//! сначала FullBox version/flags, затем тот же record. Caller обязан назвать
//! layout явно; parser никогда не отбрасывает четыре bytes эвристически.

use thiserror::Error;

use crate::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction,
    VideoCodec, VideoColorMetadata, VideoProfile, Vp8Profile, Vp9Profile,
};

/// Размер record без запрещённых для VP8/VP9 initialization bytes.
const VP_CONFIGURATION_RECORD_BASE_SIZE: usize = 8;
/// Размер version/flags prefix в FFmpeg Enhanced RTMP SequenceStart.
const FFMPEG_ENHANCED_RTMP_PREFIX_SIZE: usize = 4;
/// Нормативная текущая версия `vpcC` FullBox.
const VP_CONFIGURATION_VERSION: u8 = 1;

/// Layout codec configuration выбирается container adapter-ом явно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VpCodecConfigurationLayout {
    /// Чистый `VPCodecConfigurationRecord`, уже извлечённый из `vpcC`.
    Record,
    /// Bytes FFmpeg Enhanced RTMP SequenceStart: version/flags + record.
    FfmpegEnhancedRtmpSequenceStart,
}

/// Chroma field сохраняет различие двух нормативных 4:2:0 siting variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VpChromaSubsampling {
    /// 4:2:0 с вертикальным chroma siting.
    Yuv420Vertical,
    /// 4:2:0 chroma colocated с luma sample (0, 0).
    Yuv420Colocated,
    /// 4:2:2.
    Yuv422,
    /// 4:4:4.
    Yuv444,
}

impl VpChromaSubsampling {
    /// Сводит siting detail к общей decode-capability модели.
    #[must_use]
    pub const fn decode_subsampling(self) -> ChromaSubsampling {
        match self {
            Self::Yuv420Vertical | Self::Yuv420Colocated => ChromaSubsampling::Yuv420,
            Self::Yuv422 => ChromaSubsampling::Yuv422,
            Self::Yuv444 => ChromaSubsampling::Yuv444,
        }
    }
}

/// Валидированный VP8/VP9 configuration record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpCodecConfiguration {
    /// Codec, относительно которого проверены profile constraints.
    codec: VideoCodec,
    /// Raw profile number из record-а.
    profile: u8,
    /// Нормативный VP level number.
    level: u8,
    /// Валидированная component bit depth.
    bit_depth: BitDepth,
    /// Chroma subsampling с сохранённым 4:2:0 siting.
    chroma_subsampling: VpChromaSubsampling,
    /// Full/limited range flag.
    full_range: bool,
    /// Raw H.273 colour primaries code.
    colour_primaries: u8,
    /// Raw H.273 transfer characteristics code.
    transfer_characteristics: u8,
    /// Raw H.273 matrix coefficients code.
    matrix_coefficients: u8,
}

impl VpCodecConfiguration {
    /// Возвращает codec exact configuration-а.
    #[must_use]
    pub const fn codec(&self) -> VideoCodec {
        self.codec
    }

    /// Возвращает raw profile number.
    #[must_use]
    pub const fn profile(&self) -> u8 {
        self.profile
    }

    /// Возвращает raw level number.
    #[must_use]
    pub const fn level(&self) -> u8 {
        self.level
    }

    /// Возвращает validated bit depth.
    #[must_use]
    pub const fn bit_depth(&self) -> BitDepth {
        self.bit_depth
    }

    /// Возвращает exact chroma variant.
    #[must_use]
    pub const fn chroma_subsampling(&self) -> VpChromaSubsampling {
        self.chroma_subsampling
    }

    /// Возвращает codec-specific profile без positional integer на callsite.
    #[must_use]
    pub fn video_profile(&self) -> VideoProfile {
        match self.codec {
            VideoCodec::Vp8 => VideoProfile::Vp8(Vp8Profile::Version0To3),
            VideoCodec::Vp9 => VideoProfile::Vp9(match self.profile {
                0 => Vp9Profile::Profile0,
                1 => Vp9Profile::Profile1,
                2 => Vp9Profile::Profile2,
                3 => Vp9Profile::Profile3,
                _ => unreachable!("profile проверен constructor-ом"),
            }),
            _ => unreachable!("codec проверен constructor-ом"),
        }
    }

    /// Строит container color hint без потери raw validation на parse boundary.
    #[must_use]
    pub fn color_metadata(&self) -> VideoColorMetadata {
        VideoColorMetadata::container(
            if self.full_range {
                ColorRange::Full
            } else {
                ColorRange::Limited
            },
            MatrixCoefficients::from_h273_value(u64::from(self.matrix_coefficients)),
            ColorPrimaries::from_h273_value(u64::from(self.colour_primaries)),
            TransferFunction::from_h273_value(u64::from(self.transfer_characteristics)),
            None,
        )
    }
}

/// Typed ошибки exact VP configuration parsing/validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VpCodecConfigurationError {
    /// Parser вызван для codec вне VP8/VP9 family.
    #[error("VP codec configuration does not support {codec}")]
    UnsupportedCodec {
        /// Ошибочный codec intent caller-а.
        codec: VideoCodec,
    },
    /// Input короче обязательного layout prefix/record-а.
    #[error("VP codec configuration truncated: expected at least {required} bytes, got {actual}")]
    Truncated {
        /// Минимальная длина выбранного layout-а.
        required: usize,
        /// Фактическая длина input-а.
        actual: usize,
    },
    /// FFmpeg layout содержит неподдержанную FullBox version.
    #[error("VP codec configuration version {version} is unsupported; expected version 1")]
    UnsupportedVersion {
        /// Прочитанная версия.
        version: u8,
    },
    /// Reserved FullBox flags должны оставаться нулевыми.
    #[error("VP codec configuration reserved flags must be zero, got 0x{flags:06x}")]
    NonZeroFlags {
        /// Трёхбайтовые flags как big-endian integer.
        flags: u32,
    },
    /// Declared initialization size не совпадает с фактическим tail.
    #[error("VP codec initialization length mismatch: declared {declared} bytes, got {actual}")]
    InitializationLengthMismatch {
        /// Значение codecInitializationDataSize.
        declared: usize,
        /// Фактическая длина tail-а.
        actual: usize,
    },
    /// VP8/VP9 запрещают initialization payload.
    #[error("VP8/VP9 codec initialization data must be empty, got {size} bytes")]
    InitializationDataForbidden {
        /// Объявленная и фактически присутствующая длина.
        size: usize,
    },
    /// Profile не существует для выбранного VP codec-а.
    #[error("VP profile {profile} is invalid for {codec}")]
    InvalidProfile {
        /// Выбранный codec.
        codec: VideoCodec,
        /// Raw profile value.
        profile: u8,
    },
    /// Level отсутствует в нормативной VP9 level table.
    #[error("VP level {level} is invalid for {codec}")]
    InvalidLevel {
        /// Выбранный codec.
        codec: VideoCodec,
        /// Raw level value.
        level: u8,
    },
    /// Bit depth вне разрешённых 8/10/12.
    #[error("VP bit depth {bit_depth} is invalid")]
    InvalidBitDepth {
        /// Raw четырёхбитное значение.
        bit_depth: u8,
    },
    /// Chroma value 4..7 зарезервирован.
    #[error("VP chroma subsampling value {value} is reserved")]
    ReservedChromaSubsampling {
        /// Raw трёхбитное значение.
        value: u8,
    },
    /// Profile, depth и chroma обязаны описывать совместимый format.
    #[error(
        "VP profile {profile} is incompatible with {bit_depth}-bit chroma subsampling {chroma_subsampling}"
    )]
    ProfileFormatMismatch {
        /// Raw profile value.
        profile: u8,
        /// Raw bit depth.
        bit_depth: u8,
        /// Raw chroma value.
        chroma_subsampling: u8,
    },
    /// RGB matrix разрешена только с 4:4:4.
    #[error("VP RGB matrix requires 4:4:4 chroma subsampling")]
    RgbMatrixRequiresYuv444,
}

/// Разбирает один из двух явно названных layouts без heuristic prefix stripping.
pub fn parse_vp_codec_configuration(
    codec: VideoCodec,
    layout: VpCodecConfigurationLayout,
    input: &[u8],
) -> Result<VpCodecConfiguration, VpCodecConfigurationError> {
    if !matches!(codec, VideoCodec::Vp8 | VideoCodec::Vp9) {
        return Err(VpCodecConfigurationError::UnsupportedCodec { codec });
    }

    let record = match layout {
        VpCodecConfigurationLayout::Record => input,
        VpCodecConfigurationLayout::FfmpegEnhancedRtmpSequenceStart => {
            if input.len() < FFMPEG_ENHANCED_RTMP_PREFIX_SIZE {
                return Err(VpCodecConfigurationError::Truncated {
                    required: FFMPEG_ENHANCED_RTMP_PREFIX_SIZE,
                    actual: input.len(),
                });
            }
            if input[0] != VP_CONFIGURATION_VERSION {
                return Err(VpCodecConfigurationError::UnsupportedVersion { version: input[0] });
            }
            let flags = u32::from_be_bytes([0, input[1], input[2], input[3]]);
            if flags != 0 {
                return Err(VpCodecConfigurationError::NonZeroFlags { flags });
            }
            &input[FFMPEG_ENHANCED_RTMP_PREFIX_SIZE..]
        }
    };

    parse_vp_codec_configuration_record(codec, record)
}

/// Разбирает normative record после layout-specific boundary validation.
fn parse_vp_codec_configuration_record(
    codec: VideoCodec,
    record: &[u8],
) -> Result<VpCodecConfiguration, VpCodecConfigurationError> {
    if record.len() < VP_CONFIGURATION_RECORD_BASE_SIZE {
        return Err(VpCodecConfigurationError::Truncated {
            required: VP_CONFIGURATION_RECORD_BASE_SIZE,
            actual: record.len(),
        });
    }

    let initialization_size = usize::from(u16::from_be_bytes([record[6], record[7]]));
    let actual_initialization_size = record.len() - VP_CONFIGURATION_RECORD_BASE_SIZE;
    if initialization_size != actual_initialization_size {
        return Err(VpCodecConfigurationError::InitializationLengthMismatch {
            declared: initialization_size,
            actual: actual_initialization_size,
        });
    }
    if initialization_size != 0 {
        return Err(VpCodecConfigurationError::InitializationDataForbidden {
            size: initialization_size,
        });
    }

    let profile = record[0];
    let level = record[1];
    let packed_format = record[2];
    let raw_bit_depth = packed_format >> 4;
    let raw_chroma_subsampling = (packed_format >> 1) & 0x07;
    validate_profile(codec, profile)?;
    validate_level(codec, level)?;
    let bit_depth =
        BitDepth::from_bits(raw_bit_depth).ok_or(VpCodecConfigurationError::InvalidBitDepth {
            bit_depth: raw_bit_depth,
        })?;
    let chroma_subsampling = parse_chroma_subsampling(raw_chroma_subsampling)?;
    validate_profile_format(codec, profile, raw_bit_depth, raw_chroma_subsampling)?;
    if record[5] == 0 && chroma_subsampling != VpChromaSubsampling::Yuv444 {
        return Err(VpCodecConfigurationError::RgbMatrixRequiresYuv444);
    }

    Ok(VpCodecConfiguration {
        codec,
        profile,
        level,
        bit_depth,
        chroma_subsampling,
        full_range: packed_format & 1 != 0,
        colour_primaries: record[3],
        transfer_characteristics: record[4],
        matrix_coefficients: record[5],
    })
}

/// Проверяет codec-specific profile range.
fn validate_profile(codec: VideoCodec, profile: u8) -> Result<(), VpCodecConfigurationError> {
    let valid = match codec {
        VideoCodec::Vp8 => profile == 0,
        VideoCodec::Vp9 => profile <= 3,
        _ => false,
    };
    valid
        .then_some(())
        .ok_or(VpCodecConfigurationError::InvalidProfile { codec, profile })
}

/// Проверяет VP9 level table; VP8 не имеет level и использует zero sentinel.
fn validate_level(codec: VideoCodec, level: u8) -> Result<(), VpCodecConfigurationError> {
    const VP9_LEVELS: [u8; 15] = [0, 10, 11, 20, 21, 30, 31, 40, 41, 50, 51, 52, 60, 61, 62];
    let valid = match codec {
        VideoCodec::Vp8 => level == 0,
        VideoCodec::Vp9 => VP9_LEVELS.contains(&level),
        _ => false,
    };
    valid
        .then_some(())
        .ok_or(VpCodecConfigurationError::InvalidLevel { codec, level })
}

/// Преобразует только четыре нормативных chroma значения.
fn parse_chroma_subsampling(value: u8) -> Result<VpChromaSubsampling, VpCodecConfigurationError> {
    match value {
        0 => Ok(VpChromaSubsampling::Yuv420Vertical),
        1 => Ok(VpChromaSubsampling::Yuv420Colocated),
        2 => Ok(VpChromaSubsampling::Yuv422),
        3 => Ok(VpChromaSubsampling::Yuv444),
        _ => Err(VpCodecConfigurationError::ReservedChromaSubsampling { value }),
    }
}

/// Применяет normative profile/depth/chroma matrix для VP8 и VP9.
fn validate_profile_format(
    codec: VideoCodec,
    profile: u8,
    bit_depth: u8,
    chroma_subsampling: u8,
) -> Result<(), VpCodecConfigurationError> {
    let is_yuv420 = chroma_subsampling <= 1;
    let valid = match codec {
        VideoCodec::Vp8 => profile == 0 && bit_depth == 8 && is_yuv420,
        VideoCodec::Vp9 => match profile {
            0 => bit_depth == 8 && is_yuv420,
            1 => bit_depth == 8 && matches!(chroma_subsampling, 2 | 3),
            2 => matches!(bit_depth, 10 | 12) && is_yuv420,
            3 => matches!(bit_depth, 10 | 12) && matches!(chroma_subsampling, 2 | 3),
            _ => false,
        },
        _ => false,
    };
    valid
        .then_some(())
        .ok_or(VpCodecConfigurationError::ProfileFormatMismatch {
            profile,
            bit_depth,
            chroma_subsampling,
        })
}

#[cfg(test)]
mod tests {
    use crate::{
        BitDepth, ChromaSubsampling, ColorRange, MatrixCoefficients, VideoCodec, VideoProfile,
        Vp9Profile,
    };

    use super::{
        VpChromaSubsampling, VpCodecConfigurationError, VpCodecConfigurationLayout,
        parse_vp_codec_configuration,
    };

    /// Собирает zero-init record с заданными format fields.
    fn record(profile: u8, level: u8, depth: u8, chroma: u8, full_range: bool) -> [u8; 8] {
        [
            profile,
            level,
            (depth << 4) | (chroma << 1) | u8::from(full_range),
            9,
            16,
            9,
            0,
            0,
        ]
    }

    /// VP9 HDR record сохраняет profile/depth/chroma/color semantics.
    #[test]
    fn normative_vp9_record_is_normalized() {
        let parsed = parse_vp_codec_configuration(
            VideoCodec::Vp9,
            VpCodecConfigurationLayout::Record,
            &record(2, 10, 10, 1, true),
        )
        .expect("valid VP9 record");

        assert_eq!(
            parsed.video_profile(),
            VideoProfile::Vp9(Vp9Profile::Profile2)
        );
        assert_eq!(parsed.bit_depth(), BitDepth::Ten);
        assert_eq!(
            parsed.chroma_subsampling(),
            VpChromaSubsampling::Yuv420Colocated
        );
        assert_eq!(
            parsed.chroma_subsampling().decode_subsampling(),
            ChromaSubsampling::Yuv420
        );
        assert_eq!(parsed.color_metadata().range, ColorRange::Full);
        assert_eq!(parsed.color_metadata().matrix, MatrixCoefficients::Bt2020);
    }

    /// FFmpeg Enhanced RTMP layout требует exact version/flags prefix.
    #[test]
    fn ffmpeg_enhanced_rtmp_layout_is_explicit() {
        let mut sequence_start = vec![1, 0, 0, 0];
        sequence_start.extend_from_slice(&record(0, 0, 8, 1, false));
        let parsed = parse_vp_codec_configuration(
            VideoCodec::Vp8,
            VpCodecConfigurationLayout::FfmpegEnhancedRtmpSequenceStart,
            &sequence_start,
        )
        .expect("valid FFmpeg layout");
        assert_eq!(parsed.codec(), VideoCodec::Vp8);
        assert!(
            parse_vp_codec_configuration(
                VideoCodec::Vp8,
                VpCodecConfigurationLayout::Record,
                &sequence_start,
            )
            .is_err()
        );
    }

    /// Prefix не снимается при неизвестной version.
    #[test]
    fn unsupported_ffmpeg_layout_version_is_typed() {
        let mut sequence_start = vec![0, 0, 0, 0];
        sequence_start.extend_from_slice(&record(0, 0, 8, 1, false));
        assert_eq!(
            parse_vp_codec_configuration(
                VideoCodec::Vp8,
                VpCodecConfigurationLayout::FfmpegEnhancedRtmpSequenceStart,
                &sequence_start,
            ),
            Err(VpCodecConfigurationError::UnsupportedVersion { version: 0 })
        );
    }

    /// Codec initialization bytes запрещены binding-ом VP8/VP9.
    #[test]
    fn non_empty_initialization_data_is_rejected() {
        let mut bytes = record(0, 10, 8, 1, false).to_vec();
        bytes[7] = 1;
        bytes.push(0xaa);
        assert_eq!(
            parse_vp_codec_configuration(
                VideoCodec::Vp9,
                VpCodecConfigurationLayout::Record,
                &bytes,
            ),
            Err(VpCodecConfigurationError::InitializationDataForbidden { size: 1 })
        );
    }

    /// Profile/depth/chroma mismatch не просачивается как metadata hint.
    #[test]
    fn vp9_profile_format_mismatch_is_typed() {
        assert_eq!(
            parse_vp_codec_configuration(
                VideoCodec::Vp9,
                VpCodecConfigurationLayout::Record,
                &record(2, 10, 8, 1, false),
            ),
            Err(VpCodecConfigurationError::ProfileFormatMismatch {
                profile: 2,
                bit_depth: 8,
                chroma_subsampling: 1,
            })
        );
    }

    /// Reserved chroma value не схлопывается в generic unknown.
    #[test]
    fn reserved_chroma_is_typed() {
        assert_eq!(
            parse_vp_codec_configuration(
                VideoCodec::Vp9,
                VpCodecConfigurationLayout::Record,
                &record(0, 10, 8, 4, false),
            ),
            Err(VpCodecConfigurationError::ReservedChromaSubsampling { value: 4 })
        );
    }
}
