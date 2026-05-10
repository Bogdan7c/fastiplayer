use serde::{Deserialize, Serialize};

use crate::{
    BitDepth, ChromaSubsampling, VideoCodec, VideoDecodeRequirement, VideoProfile, Vp9Profile,
};

/// Результат VP9 packet/header probing для capability layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9RequirementProbe {
    /// Header надёжно распознан и дал requirement, который можно сверять с backend/render matrix.
    Candidate(Vp9RequirementCandidate),

    /// Header надёжно распознан, но вариант VP9 выходит за production policy Phase 9.
    Rejected(Vp9RequirementRejection),

    /// Header нельзя использовать для строгого отказа: decoder должен получить packet и продолжить.
    Recoverable(Vp9RequirementUncertainty),
}

/// Поддержанный или потенциально поддержанный VP9 requirement из надёжного header-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Vp9RequirementCandidate {
    /// Нормализованное требование к hardware decoder-у.
    pub requirement: VideoDecodeRequirement,

    /// Ожидаемый decoded surface format на границе decoder/renderer.
    pub decoded_format: Vp9DecodedFormatRequirement,
}

/// Ожидаемый decoded format для VP9 variant-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9DecodedFormatRequirement {
    /// 8-bit 4:2:0 path, который renderer уже принимает как NV12.
    Nv12,

    /// 10-bit 4:2:0 path, который Phase 9 распознаёт как P010 boundary candidate.
    P010,
}

/// Строгий VP9 policy reject, полученный из валидного bitstream header-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9RequirementRejection {
    /// VP9 12-bit требует P012/12-bit path, которого нет в scope Phase 9.
    UnsupportedBitDepth(u8),

    /// VP9 4:2:2/4:4:4 не поддерживается текущим production renderer/decode contract.
    UnsupportedChroma(ChromaSubsampling),
}

impl Vp9RequirementRejection {
    /// Возвращает user-facing diagnostic без привязки к конкретному hardware backend-у.
    #[must_use]
    pub fn user_message(self) -> String {
        match self {
            Self::UnsupportedBitDepth(bit_depth) => {
                format!("VP9 {bit_depth}-bit не поддерживается: P012/12-bit path вне scope Phase 9")
            }
            Self::UnsupportedChroma(chroma) => format!(
                "VP9 chroma/profile combination {chroma} не поддерживается: production path принимает только VP9 Profile 0/2 4:2:0"
            ),
        }
    }
}

/// Причина, по которой VP9 packet нельзя использовать для строгого capability reject-а.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9RequirementUncertainty {
    /// Parser не смог надёжно разобрать header; это diagnostic, а не hardware reject.
    ParseError(String),

    /// Packet только показывает уже декодированный кадр и не несёт stream requirement.
    ShowExistingFrame,

    /// Inter/non-keyframe не содержит color config, поэтому profile/format ещё нельзя фиксировать.
    MissingColorConfig {
        /// Profile уже прочитан из packet header-а.
        profile: Vp9Profile,
    },

    /// Header содержит chroma flag combination, которой нет в текущей typed model.
    UnknownChromaLayout {
        /// VP9 subsampling_x flag из header-а.
        subsampling_x: bool,

        /// VP9 subsampling_y flag из header-а.
        subsampling_y: bool,
    },

    /// Header содержит bit depth вне известной VP9 matrix.
    UnknownBitDepth {
        /// Сырое значение bit depth из parser-а.
        bit_depth: u8,
    },
}

/// Читает VP9 uncompressed header и строит Phase 9 requirement decision.
#[must_use]
pub fn probe_vp9_packet_requirement(packet_bytes: &[u8]) -> Vp9RequirementProbe {
    match vp9_parser::parse_uncompressed_header(packet_bytes) {
        Ok(frame_info) => requirement_probe_from_frame_info(frame_info),
        Err(error) => Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::ParseError(
            error.to_string(),
        )),
    }
}

/// Переводит уже распарсенный VP9 header в candidate/reject/recoverable model.
fn requirement_probe_from_frame_info(frame_info: vp9_parser::Vp9FrameInfo) -> Vp9RequirementProbe {
    if frame_info.show_existing_frame {
        return Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::ShowExistingFrame);
    }

    let Some(profile) = Vp9Profile::from_bitstream_value(frame_info.profile) else {
        return Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::ParseError(format!(
            "unsupported VP9 profile {}",
            frame_info.profile
        )));
    };

    let Some(bit_depth) = frame_info.bit_depth else {
        return Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::MissingColorConfig {
            profile,
        });
    };

    let Some(chroma) =
        chroma_from_subsampling_flags(frame_info.subsampling_x, frame_info.subsampling_y)
    else {
        return recoverable_chroma_uncertainty(frame_info.subsampling_x, frame_info.subsampling_y);
    };

    if matches!(profile, Vp9Profile::Profile1 | Vp9Profile::Profile3)
        || matches!(
            chroma,
            ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444
        )
    {
        return Vp9RequirementProbe::Rejected(Vp9RequirementRejection::UnsupportedChroma(chroma));
    }

    let Some(bit_depth_model) = bit_depth_from_header(bit_depth) else {
        return recoverable_bit_depth_uncertainty(bit_depth);
    };

    let Some(decoded_format) = decoded_format_from_supported_bit_depth(bit_depth_model) else {
        return Vp9RequirementProbe::Rejected(Vp9RequirementRejection::UnsupportedBitDepth(
            bit_depth,
        ));
    };

    let mut requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(profile))
        .with_bit_depth(bit_depth_model)
        .with_chroma(chroma);

    if frame_info.width > 0 && frame_info.height > 0 {
        requirement = requirement.with_resolution(frame_info.width, frame_info.height);
    }

    Vp9RequirementProbe::Candidate(Vp9RequirementCandidate {
        requirement,
        decoded_format,
    })
}

/// Переводит сырое VP9 bit depth значение в typed model.
fn bit_depth_from_header(bit_depth: u8) -> Option<BitDepth> {
    match bit_depth {
        8 => Some(BitDepth::Eight),
        10 => Some(BitDepth::Ten),
        12 => Some(BitDepth::Twelve),
        _ => None,
    }
}

/// Выводит decoded format только для bit depths, допущенных Phase 9 adapter-ом.
fn decoded_format_from_supported_bit_depth(
    bit_depth: BitDepth,
) -> Option<Vp9DecodedFormatRequirement> {
    match bit_depth {
        BitDepth::Eight => Some(Vp9DecodedFormatRequirement::Nv12),
        BitDepth::Ten => Some(Vp9DecodedFormatRequirement::P010),
        BitDepth::Twelve => None,
    }
}

/// Переводит VP9 subsampling flags в общую chroma model.
fn chroma_from_subsampling_flags(
    subsampling_x: Option<bool>,
    subsampling_y: Option<bool>,
) -> Option<ChromaSubsampling> {
    match (subsampling_x, subsampling_y) {
        (Some(true), Some(true)) => Some(ChromaSubsampling::Yuv420),
        (Some(true), Some(false)) => Some(ChromaSubsampling::Yuv422),
        (Some(false), Some(false)) => Some(ChromaSubsampling::Yuv444),
        _ => None,
    }
}

/// Создаёт recoverable результат для неизвестной chroma layout.
fn recoverable_chroma_uncertainty(
    subsampling_x: Option<bool>,
    subsampling_y: Option<bool>,
) -> Vp9RequirementProbe {
    match (subsampling_x, subsampling_y) {
        (Some(subsampling_x), Some(subsampling_y)) => {
            Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::UnknownChromaLayout {
                subsampling_x,
                subsampling_y,
            })
        }
        _ => Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::ParseError(
            "VP9 color config did not expose chroma subsampling".to_string(),
        )),
    }
}

/// Создаёт recoverable результат для неизвестного bit depth.
fn recoverable_bit_depth_uncertainty(bit_depth: u8) -> Vp9RequirementProbe {
    Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::UnknownBitDepth { bit_depth })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME_MARKER: u32 = 0b10;
    const SYNC_CODE: u32 = 0x498342;

    #[test]
    fn profile0_8bit_yuv420_builds_nv12_requirement() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 0,
            bit_depth: 8,
            subsampling_x: true,
            subsampling_y: true,
            width: 64,
            height: 64,
        });

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        let Vp9RequirementProbe::Candidate(candidate) = probe else {
            panic!("profile0 8-bit 4:2:0 должен дать candidate, получено {probe:?}");
        };
        assert_eq!(candidate.decoded_format, Vp9DecodedFormatRequirement::Nv12);
        assert_eq!(
            candidate.requirement.profile,
            Some(VideoProfile::Vp9(Vp9Profile::Profile0))
        );
        assert_eq!(candidate.requirement.bit_depth, Some(BitDepth::Eight));
        assert_eq!(
            candidate.requirement.chroma,
            Some(ChromaSubsampling::Yuv420)
        );
    }

    #[test]
    fn profile2_10bit_yuv420_builds_p010_candidate_requirement() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 10,
            subsampling_x: true,
            subsampling_y: true,
            width: 128,
            height: 72,
        });

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        let Vp9RequirementProbe::Candidate(candidate) = probe else {
            panic!("profile2 10-bit 4:2:0 должен дать P010 candidate, получено {probe:?}");
        };
        assert_eq!(candidate.decoded_format, Vp9DecodedFormatRequirement::P010);
        assert_eq!(
            candidate.requirement.profile,
            Some(VideoProfile::Vp9(Vp9Profile::Profile2))
        );
        assert_eq!(candidate.requirement.bit_depth, Some(BitDepth::Ten));
        assert_eq!(
            candidate.requirement.chroma,
            Some(ChromaSubsampling::Yuv420)
        );
    }

    #[test]
    fn profile2_12bit_is_explicit_unsupported_bit_depth() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 12,
            subsampling_x: true,
            subsampling_y: true,
            width: 128,
            height: 72,
        });

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        assert_eq!(
            probe,
            Vp9RequirementProbe::Rejected(Vp9RequirementRejection::UnsupportedBitDepth(12))
        );
    }

    #[test]
    fn profile1_yuv422_is_explicit_unsupported_chroma() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 1,
            bit_depth: 8,
            subsampling_x: true,
            subsampling_y: false,
            width: 128,
            height: 72,
        });

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        assert_eq!(
            probe,
            Vp9RequirementProbe::Rejected(Vp9RequirementRejection::UnsupportedChroma(
                ChromaSubsampling::Yuv422
            ))
        );
    }

    #[test]
    fn profile3_yuv444_is_explicit_unsupported_chroma() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 3,
            bit_depth: 10,
            subsampling_x: false,
            subsampling_y: false,
            width: 128,
            height: 72,
        });

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        assert_eq!(
            probe,
            Vp9RequirementProbe::Rejected(Vp9RequirementRejection::UnsupportedChroma(
                ChromaSubsampling::Yuv444
            ))
        );
    }

    #[test]
    fn incomplete_packet_is_recoverable() {
        let probe = probe_vp9_packet_requirement(&[0x00]);

        assert!(matches!(
            probe,
            Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::ParseError(_))
        ));
    }

    #[test]
    fn inter_frame_without_color_config_is_recoverable() {
        let packet_bytes = build_inter_frame_without_refs();

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        assert_eq!(
            probe,
            Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::MissingColorConfig {
                profile: Vp9Profile::Profile0
            })
        );
    }

    struct Vp9HeaderFixture {
        profile: u8,
        bit_depth: u8,
        subsampling_x: bool,
        subsampling_y: bool,
        width: u32,
        height: u32,
    }

    fn build_vp9_keyframe(fixture: Vp9HeaderFixture) -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, FRAME_MARKER, 2);
        push_profile(&mut bits, fixture.profile);
        bits.push(0);
        bits.push(0);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, SYNC_CODE, 24);
        if matches!(fixture.profile, 2 | 3) {
            bits.push(u8::from(fixture.bit_depth == 12));
        }
        push_bits(&mut bits, 1, 3);
        bits.push(0);
        if matches!(fixture.profile, 1 | 3) {
            bits.push(u8::from(fixture.subsampling_x));
            bits.push(u8::from(fixture.subsampling_y));
            bits.push(0);
        }
        push_bits(&mut bits, fixture.width - 1, 16);
        push_bits(&mut bits, fixture.height - 1, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    fn build_inter_frame_without_refs() -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, FRAME_MARKER, 2);
        push_profile(&mut bits, 0);
        bits.push(0);
        bits.push(1);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, 0, 2);
        push_bits(&mut bits, 0x01, 8);
        push_bits(&mut bits, 1, 3);
        bits.push(1);
        push_bits(&mut bits, 2, 3);
        bits.push(0);
        push_bits(&mut bits, 3, 3);
        bits.push(1);
        bits.push(0);
        bits.push(0);
        bits.push(0);
        push_bits(&mut bits, 63, 16);
        push_bits(&mut bits, 63, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                let mut byte = 0u8;
                for (index, bit) in chunk.iter().enumerate() {
                    byte |= bit << (7 - index);
                }
                byte
            })
            .collect()
    }

    fn push_bits(bits: &mut Vec<u8>, value: u32, width: u8) {
        for shift in (0..width).rev() {
            bits.push(((value >> shift) & 1) as u8);
        }
    }

    fn push_profile(bits: &mut Vec<u8>, profile: u8) {
        bits.push(profile & 1);
        bits.push((profile >> 1) & 1);
        if profile == 3 {
            bits.push(0);
        }
    }
}
