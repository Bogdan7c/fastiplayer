//! VA-API capability probing без запуска playback pipeline.
//!
//! Модуль читает libva profiles/entrypoints/RT formats и переводит их в
//! `codec-core` matrix. Decode thread остаётся отдельно: probe не создаёт
//! decoder, frame pool, graphics device или renderer resources.

use std::collections::BTreeSet;
use std::rc::Rc;

use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, DmaBufImageLayout, DriverQuirk,
    VideoCapabilityProvider, VideoExportPath,
};
use codec_core::{
    Av1Profile, BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, H265Profile,
    SupportedVideoDecodeFormat, VideoCodec, VideoProfile, Vp8Profile, Vp9Profile,
};
use cros_codecs::libva;
use tracing::{debug, warn};

use crate::codec_adapter::VaapiCodecAdapterFactory;

/// Человекочитаемое имя backend-а в reports.
const VAAPI_DISPLAY_NAME: &str = "VA-API";

/// Decode entrypoint, который нужен именно для аппаратного video decode.
const DECODE_ENTRYPOINT_LABEL: &str = "VLD";

/// Provider VA-API capabilities для `capability-core`.
#[derive(Debug, Clone, Copy, Default)]
pub struct VaapiCapabilityProvider;

impl VaapiCapabilityProvider {
    /// Создаёт stateless provider.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl VideoCapabilityProvider for VaapiCapabilityProvider {
    /// Возвращает canonical id VA-API backend-а.
    fn backend_id(&self) -> DecodeBackendId {
        DecodeBackendId::vaapi()
    }

    /// Выполняет libva probe и возвращает typed backend report.
    fn probe(&self) -> BackendCapabilities {
        probe_vaapi_capabilities()
    }
}

/// Выполняет VA-API probe без создания decoder thread.
#[must_use]
pub fn probe_vaapi_capabilities() -> BackendCapabilities {
    let backend_id = DecodeBackendId::vaapi();
    let Some(display) = libva::Display::open() else {
        return BackendCapabilities::unavailable(
            backend_id,
            VAAPI_DISPLAY_NAME,
            "libva не смог открыть ни один DRM render node",
        );
    };

    let vendor = match display.query_vendor_string() {
        Ok(vendor) => Some(vendor),
        Err(error) => {
            warn!(error, "VA-API vendor string недоступен");
            None
        }
    };
    let driver = BackendDriverInfo {
        driver_name: vendor.as_deref().and_then(normalize_driver_name),
        vendor: vendor.clone(),
        device_name: None,
    };
    let quirks = driver
        .driver_name
        .as_deref()
        .map(known_driver_quirks)
        .unwrap_or_default();

    let profiles = match display.query_config_profiles() {
        Ok(profiles) => profiles,
        Err(error) => {
            return BackendCapabilities::unavailable(
                backend_id,
                VAAPI_DISPLAY_NAME,
                format!("vaQueryConfigProfiles failed: {error}"),
            );
        }
    };

    let mut raw_profiles = BTreeSet::new();
    let mut raw_entrypoints = BTreeSet::new();
    let mut raw_rt_formats = BTreeSet::new();
    let mut diagnostics = Vec::new();
    let mut supported_formats = Vec::new();

    for profile in profiles {
        raw_profiles.insert(profile_label(profile));

        let entrypoints = match display.query_config_entrypoints(profile) {
            Ok(entrypoints) => entrypoints,
            Err(error) => {
                diagnostics.push(format!(
                    "{}: vaQueryConfigEntrypoints failed: {error}",
                    profile_label(profile)
                ));
                continue;
            }
        };

        for entrypoint in &entrypoints {
            raw_entrypoints.insert(entrypoint_label(*entrypoint));
        }

        if !entrypoints.contains(&libva::VAEntrypoint::VAEntrypointVLD) {
            continue;
        }

        let rt_format_mask =
            match query_rt_format_mask(&display, profile, libva::VAEntrypoint::VAEntrypointVLD) {
                Some(rt_format_mask) => rt_format_mask,
                None => {
                    diagnostics.push(format!(
                        "{}: {DECODE_ENTRYPOINT_LABEL} не сообщил RT format",
                        profile_label(profile)
                    ));
                    continue;
                }
            };

        for label in rt_format_labels_from_mask(rt_format_mask) {
            raw_rt_formats.insert(label);
        }

        let max_resolution = query_max_resolution(
            &display,
            profile,
            libva::VAEntrypoint::VAEntrypointVLD,
            rt_format_mask,
        );
        supported_formats.extend(formats_for_va_profile(
            profile,
            rt_format_mask,
            max_resolution,
        ));
    }

    supported_formats.sort_by(|left, right| {
        left.codec
            .cmp(&right.codec)
            .then(left.profile.cmp(&right.profile))
            .then(left.bit_depth.cmp(&right.bit_depth))
            .then(left.chroma.cmp(&right.chroma))
    });
    supported_formats.dedup_by(|left, right| {
        left.codec == right.codec
            && left.profile == right.profile
            && left.bit_depth == right.bit_depth
            && left.chroma == right.chroma
            && left.max_width == right.max_width
            && left.max_height == right.max_height
    });

    debug!(
        formats = supported_formats.len(),
        vendor = vendor.as_deref().unwrap_or("unknown"),
        "VA-API capability probe completed"
    );

    let p010_storage_layouts = p010_storage_layouts_for_formats(&supported_formats);

    BackendCapabilities {
        backend_id,
        display_name: VAAPI_DISPLAY_NAME.to_string(),
        status: BackendProbeStatus::Available,
        driver,
        supported_video_decode_formats: supported_formats,
        raw_profiles: raw_profiles.into_iter().collect(),
        raw_entrypoints: raw_entrypoints.into_iter().collect(),
        raw_rt_formats: raw_rt_formats.into_iter().collect(),
        quirks,
        export_paths: vec![VideoExportPath::DmaBuf],
        p010_storage_layouts,
        diagnostics,
    }
}

/// Возвращает P010 export layouts только если decode matrix реально содержит P010-кандидат.
fn p010_storage_layouts_for_formats(
    supported_formats: &[SupportedVideoDecodeFormat],
) -> Vec<DmaBufImageLayout> {
    let has_p010_format = supported_formats.iter().any(|format| {
        format.bit_depth == BitDepth::Ten && format.chroma == ChromaSubsampling::Yuv420
    });

    if has_p010_format {
        vec![DmaBufImageLayout::SeparateLayers]
    } else {
        Vec::new()
    }
}

/// Максимальное coded resolution, если libva смогло его сообщить.
#[derive(Debug, Clone, Copy)]
struct MaxResolution {
    /// Максимальная coded width.
    width: Option<u32>,

    /// Максимальная coded height.
    height: Option<u32>,
}

/// Запрашивает RT format mask для пары profile/entrypoint.
fn query_rt_format_mask(
    display: &libva::Display,
    profile: libva::VAProfile::Type,
    entrypoint: libva::VAEntrypoint::Type,
) -> Option<u32> {
    let mut attributes = vec![libva::VAConfigAttrib {
        type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
        value: 0,
    }];

    if let Err(error) = display.get_config_attributes(profile, entrypoint, &mut attributes) {
        warn!(
            error = %error,
            profile = %profile_label(profile),
            entrypoint = %entrypoint_label(entrypoint),
            "VA-API RT format query failed"
        );
        return None;
    }

    let rt_format_mask = attributes[0].value;
    if rt_format_mask == libva::VA_ATTRIB_NOT_SUPPORTED {
        None
    } else {
        Some(rt_format_mask)
    }
}

/// Пытается прочитать MaxWidth/MaxHeight из VA surface attributes.
fn query_max_resolution(
    display: &Rc<libva::Display>,
    profile: libva::VAProfile::Type,
    entrypoint: libva::VAEntrypoint::Type,
    rt_format_mask: u32,
) -> MaxResolution {
    let Some(first_rt_format) = first_rt_format_bit(rt_format_mask) else {
        return MaxResolution {
            width: None,
            height: None,
        };
    };

    let attributes = vec![libva::VAConfigAttrib {
        type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
        value: first_rt_format,
    }];

    let Ok(mut config) = display.create_config(attributes, profile, entrypoint) else {
        return MaxResolution {
            width: None,
            height: None,
        };
    };

    MaxResolution {
        width: query_surface_integer_attribute(
            &mut config,
            libva::VASurfaceAttribType::VASurfaceAttribMaxWidth,
        ),
        height: query_surface_integer_attribute(
            &mut config,
            libva::VASurfaceAttribType::VASurfaceAttribMaxHeight,
        ),
    }
}

/// Читает integer surface attribute и отбрасывает нечисловые значения.
fn query_surface_integer_attribute(
    config: &mut libva::Config,
    attribute_type: libva::VASurfaceAttribType::Type,
) -> Option<u32> {
    let values = config
        .query_surface_attributes_by_type(attribute_type)
        .ok()?;

    values.into_iter().find_map(|value| match value {
        libva::GenericValue::Integer(value) if value > 0 => Some(value as u32),
        _ => None,
    })
}

/// Возвращает первый установленный RT format bit из mask.
fn first_rt_format_bit(rt_format_mask: u32) -> Option<u32> {
    rt_format_specs()
        .iter()
        .map(|spec| spec.rt_format)
        .find(|rt_format| rt_format_mask & *rt_format != 0)
}

/// Один RT format и его typed codec-core смысл.
#[derive(Debug, Clone, Copy)]
struct RtFormatSpec {
    /// libva RT format bit.
    rt_format: u32,

    /// Bit depth этого RT format.
    bit_depth: BitDepth,

    /// Chroma subsampling этого RT format.
    chroma: ChromaSubsampling,

    /// Label для diagnostics.
    label: &'static str,
}

/// Возвращает известные RT formats, которые важны decode matrix.
fn rt_format_specs() -> &'static [RtFormatSpec] {
    &[
        RtFormatSpec {
            rt_format: libva::VA_RT_FORMAT_YUV420,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            label: "YUV420",
        },
        RtFormatSpec {
            rt_format: libva::VA_RT_FORMAT_YUV420_10,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
            label: "YUV420_10",
        },
        RtFormatSpec {
            rt_format: libva::VA_RT_FORMAT_YUV420_12,
            bit_depth: BitDepth::Twelve,
            chroma: ChromaSubsampling::Yuv420,
            label: "YUV420_12",
        },
        RtFormatSpec {
            rt_format: libva::VA_RT_FORMAT_YUV422,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv422,
            label: "YUV422",
        },
        RtFormatSpec {
            rt_format: libva::VA_RT_FORMAT_YUV422_10,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv422,
            label: "YUV422_10",
        },
        RtFormatSpec {
            rt_format: libva::VA_RT_FORMAT_YUV422_12,
            bit_depth: BitDepth::Twelve,
            chroma: ChromaSubsampling::Yuv422,
            label: "YUV422_12",
        },
        RtFormatSpec {
            rt_format: libva::VA_RT_FORMAT_YUV444,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv444,
            label: "YUV444",
        },
        RtFormatSpec {
            rt_format: libva::VA_RT_FORMAT_YUV444_10,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv444,
            label: "YUV444_10",
        },
        RtFormatSpec {
            rt_format: libva::VA_RT_FORMAT_YUV444_12,
            bit_depth: BitDepth::Twelve,
            chroma: ChromaSubsampling::Yuv444,
            label: "YUV444_12",
        },
    ]
}

/// Возвращает label всех RT formats, установленных в mask.
fn rt_format_labels_from_mask(rt_format_mask: u32) -> Vec<String> {
    rt_format_specs()
        .iter()
        .filter(|spec| rt_format_mask & spec.rt_format != 0)
        .map(|spec| spec.label.to_string())
        .collect()
}

/// Строит typed decode formats для одного VA profile.
fn formats_for_va_profile(
    profile: libva::VAProfile::Type,
    rt_format_mask: u32,
    max_resolution: MaxResolution,
) -> Vec<SupportedVideoDecodeFormat> {
    let mut formats = Vec::new();

    match profile {
        libva::VAProfile::VAProfileVP9Profile0 => {
            push_matching_rt_format(
                &mut formats,
                rt_format_mask,
                VideoCodec::Vp9,
                VideoProfile::Vp9(Vp9Profile::Profile0),
                &[ChromaSubsampling::Yuv420],
                &[BitDepth::Eight],
                max_resolution,
            );
        }
        libva::VAProfile::VAProfileVP9Profile1 => {
            push_matching_rt_format(
                &mut formats,
                rt_format_mask,
                VideoCodec::Vp9,
                VideoProfile::Vp9(Vp9Profile::Profile1),
                &[ChromaSubsampling::Yuv422, ChromaSubsampling::Yuv444],
                &[BitDepth::Eight],
                max_resolution,
            );
        }
        libva::VAProfile::VAProfileVP9Profile2 => {
            push_matching_rt_format(
                &mut formats,
                rt_format_mask,
                VideoCodec::Vp9,
                VideoProfile::Vp9(Vp9Profile::Profile2),
                &[ChromaSubsampling::Yuv420],
                &[BitDepth::Ten, BitDepth::Twelve],
                max_resolution,
            );
        }
        libva::VAProfile::VAProfileVP9Profile3 => {
            push_matching_rt_format(
                &mut formats,
                rt_format_mask,
                VideoCodec::Vp9,
                VideoProfile::Vp9(Vp9Profile::Profile3),
                &[ChromaSubsampling::Yuv422, ChromaSubsampling::Yuv444],
                &[BitDepth::Ten, BitDepth::Twelve],
                max_resolution,
            );
        }
        libva::VAProfile::VAProfileAV1Profile0 => {
            push_matching_rt_format(
                &mut formats,
                rt_format_mask,
                VideoCodec::Av1,
                VideoProfile::Av1(Av1Profile::Main),
                &[ChromaSubsampling::Yuv420],
                &[BitDepth::Eight, BitDepth::Ten],
                max_resolution,
            );
        }
        libva::VAProfile::VAProfileAV1Profile1 => {
            push_matching_rt_format(
                &mut formats,
                rt_format_mask,
                VideoCodec::Av1,
                VideoProfile::Av1(Av1Profile::High),
                &[ChromaSubsampling::Yuv444],
                &[BitDepth::Eight, BitDepth::Ten],
                max_resolution,
            );
        }
        libva::VAProfile::VAProfileH264ConstrainedBaseline => {
            push_matching_rt_format(
                &mut formats,
                rt_format_mask,
                VideoCodec::H264,
                VideoProfile::H264(H264Profile::ConstrainedBaseline),
                &[ChromaSubsampling::Yuv420],
                &[BitDepth::Eight],
                max_resolution,
            );
        }
        libva::VAProfile::VAProfileH264Main => {
            push_matching_rt_format(
                &mut formats,
                rt_format_mask,
                VideoCodec::H264,
                VideoProfile::H264(H264Profile::Main),
                &[ChromaSubsampling::Yuv420],
                &[BitDepth::Eight],
                max_resolution,
            );
        }
        libva::VAProfile::VAProfileH264High => {
            push_matching_rt_format(
                &mut formats,
                rt_format_mask,
                VideoCodec::H264,
                VideoProfile::H264(H264Profile::High),
                &[
                    ChromaSubsampling::Yuv420,
                    ChromaSubsampling::Yuv422,
                    ChromaSubsampling::Yuv444,
                ],
                &[BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve],
                max_resolution,
            );
        }
        libva::VAProfile::VAProfileHEVCMain => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::Main),
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCMain10 => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::Main10),
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCMain12 => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::Main12),
            BitDepth::Twelve,
            ChromaSubsampling::Yuv420,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCMain422_10 => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::Main422_10),
            BitDepth::Ten,
            ChromaSubsampling::Yuv422,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCMain422_12 => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::Main422_12),
            BitDepth::Twelve,
            ChromaSubsampling::Yuv422,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCMain444 => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::Main444),
            BitDepth::Eight,
            ChromaSubsampling::Yuv444,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCMain444_10 => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::Main444_10),
            BitDepth::Ten,
            ChromaSubsampling::Yuv444,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCMain444_12 => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::Main444_12),
            BitDepth::Twelve,
            ChromaSubsampling::Yuv444,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCSccMain => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::SccMain),
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCSccMain10 => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::SccMain10),
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCSccMain444 => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::SccMain444),
            BitDepth::Eight,
            ChromaSubsampling::Yuv444,
            max_resolution,
        ),
        libva::VAProfile::VAProfileHEVCSccMain444_10 => push_single_hevc_profile(
            &mut formats,
            rt_format_mask,
            VideoProfile::H265(H265Profile::SccMain444_10),
            BitDepth::Ten,
            ChromaSubsampling::Yuv444,
            max_resolution,
        ),
        libva::VAProfile::VAProfileVP8Version0_3 => {
            push_matching_rt_format(
                &mut formats,
                rt_format_mask,
                VideoCodec::Vp8,
                VideoProfile::Vp8(Vp8Profile::Version0To3),
                &[ChromaSubsampling::Yuv420],
                &[BitDepth::Eight],
                max_resolution,
            );
        }
        _ => {}
    }

    // Hardware profile сам по себе не означает production support. Report должен
    // показывать только formats, для которых в `video-vaapi` уже есть adapter,
    // умеющий сохранить zero-copy decode path без CPU fallback.
    formats.retain(VaapiCodecAdapterFactory::supports_decode_format);

    formats
}

/// Добавляет HEVC profile с одним ожидаемым bit depth/chroma.
fn push_single_hevc_profile(
    formats: &mut Vec<SupportedVideoDecodeFormat>,
    rt_format_mask: u32,
    profile: VideoProfile,
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
    max_resolution: MaxResolution,
) {
    push_matching_rt_format(
        formats,
        rt_format_mask,
        VideoCodec::H265,
        profile,
        &[chroma],
        &[bit_depth],
        max_resolution,
    );
}

/// Добавляет все RT formats, совместимые с codec/profile filter.
fn push_matching_rt_format(
    formats: &mut Vec<SupportedVideoDecodeFormat>,
    rt_format_mask: u32,
    codec: VideoCodec,
    profile: VideoProfile,
    allowed_chroma: &[ChromaSubsampling],
    allowed_bit_depth: &[BitDepth],
    max_resolution: MaxResolution,
) {
    for spec in rt_format_specs() {
        if rt_format_mask & spec.rt_format == 0 {
            continue;
        }

        if !allowed_chroma.contains(&spec.chroma) || !allowed_bit_depth.contains(&spec.bit_depth) {
            continue;
        }

        formats.push(SupportedVideoDecodeFormat {
            codec,
            profile,
            bit_depth: spec.bit_depth,
            chroma: spec.chroma,
            max_width: max_resolution.width,
            max_height: max_resolution.height,
            max_fps: None,
            hdr_input: matches!(spec.bit_depth, BitDepth::Ten | BitDepth::Twelve),
            backend: DecodeBackendId::vaapi(),
        });
    }
}

/// Формирует label VA profile без зависимости от Debug вывода bindgen.
fn profile_label(profile: libva::VAProfile::Type) -> String {
    match profile {
        libva::VAProfile::VAProfileVP9Profile0 => "VAProfileVP9Profile0".to_string(),
        libva::VAProfile::VAProfileVP9Profile1 => "VAProfileVP9Profile1".to_string(),
        libva::VAProfile::VAProfileVP9Profile2 => "VAProfileVP9Profile2".to_string(),
        libva::VAProfile::VAProfileVP9Profile3 => "VAProfileVP9Profile3".to_string(),
        libva::VAProfile::VAProfileAV1Profile0 => "VAProfileAV1Profile0".to_string(),
        libva::VAProfile::VAProfileAV1Profile1 => "VAProfileAV1Profile1".to_string(),
        libva::VAProfile::VAProfileH264ConstrainedBaseline => {
            "VAProfileH264ConstrainedBaseline".to_string()
        }
        libva::VAProfile::VAProfileH264Main => "VAProfileH264Main".to_string(),
        libva::VAProfile::VAProfileH264High => "VAProfileH264High".to_string(),
        libva::VAProfile::VAProfileHEVCMain => "VAProfileHEVCMain".to_string(),
        libva::VAProfile::VAProfileHEVCMain10 => "VAProfileHEVCMain10".to_string(),
        libva::VAProfile::VAProfileHEVCMain12 => "VAProfileHEVCMain12".to_string(),
        libva::VAProfile::VAProfileHEVCMain422_10 => "VAProfileHEVCMain422_10".to_string(),
        libva::VAProfile::VAProfileHEVCMain422_12 => "VAProfileHEVCMain422_12".to_string(),
        libva::VAProfile::VAProfileHEVCMain444 => "VAProfileHEVCMain444".to_string(),
        libva::VAProfile::VAProfileHEVCMain444_10 => "VAProfileHEVCMain444_10".to_string(),
        libva::VAProfile::VAProfileHEVCMain444_12 => "VAProfileHEVCMain444_12".to_string(),
        libva::VAProfile::VAProfileHEVCSccMain => "VAProfileHEVCSccMain".to_string(),
        libva::VAProfile::VAProfileHEVCSccMain10 => "VAProfileHEVCSccMain10".to_string(),
        libva::VAProfile::VAProfileHEVCSccMain444 => "VAProfileHEVCSccMain444".to_string(),
        libva::VAProfile::VAProfileHEVCSccMain444_10 => "VAProfileHEVCSccMain444_10".to_string(),
        libva::VAProfile::VAProfileVP8Version0_3 => "VAProfileVP8Version0_3".to_string(),
        _ => format!("VAProfile({profile})"),
    }
}

/// Формирует label VA entrypoint.
fn entrypoint_label(entrypoint: libva::VAEntrypoint::Type) -> String {
    match entrypoint {
        libva::VAEntrypoint::VAEntrypointVLD => DECODE_ENTRYPOINT_LABEL.to_string(),
        libva::VAEntrypoint::VAEntrypointEncSlice => "EncSlice".to_string(),
        libva::VAEntrypoint::VAEntrypointEncSliceLP => "EncSliceLP".to_string(),
        libva::VAEntrypoint::VAEntrypointVideoProc => "VideoProc".to_string(),
        _ => format!("VAEntrypoint({entrypoint})"),
    }
}

/// Нормализует vendor string в driver family.
fn normalize_driver_name(vendor: &str) -> Option<String> {
    let vendor_lowercase = vendor.to_ascii_lowercase();

    if vendor_lowercase.contains("intel")
        && (vendor_lowercase.contains("media driver") || vendor_lowercase.contains("ihd"))
    {
        return Some("intel-ihd".to_string());
    }

    if vendor_lowercase.contains("i965") {
        return Some("intel-i965".to_string());
    }

    if vendor_lowercase.contains("mesa") {
        return Some("mesa".to_string());
    }

    if vendor.trim().is_empty() {
        None
    } else {
        Some("other".to_string())
    }
}

/// Возвращает known quirks для driver family.
fn known_driver_quirks(driver_name: &str) -> Vec<DriverQuirk> {
    match driver_name {
        "intel-i965" => vec![DriverQuirk {
            id: "legacy-intel-i965".to_string(),
            description:
                "Старый Intel VA-API driver; VP9/AV1 support может быть ограничен, H.264 важнее для legacy playback"
                    .to_string(),
        }],
        "intel-ihd" => vec![DriverQuirk {
            id: "intel-ihd-modern".to_string(),
            description: "Современный Intel media driver; обычно основной путь для VP9/AV1"
                .to_string(),
        }],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intel_vendor_string_is_normalized_to_driver_family() {
        assert_eq!(
            normalize_driver_name("Intel iHD driver for Intel(R) Gen Graphics"),
            Some("intel-ihd".to_string())
        );
        assert_eq!(
            normalize_driver_name("Intel i965 driver for Intel(R) Broadwell"),
            Some("intel-i965".to_string())
        );
        assert_eq!(
            normalize_driver_name("Mesa Gallium driver"),
            Some("mesa".to_string())
        );
    }

    #[test]
    fn vp9_profile0_yuv420_mask_builds_profile0_format() {
        let formats = formats_for_va_profile(
            libva::VAProfile::VAProfileVP9Profile0,
            libva::VA_RT_FORMAT_YUV420,
            MaxResolution {
                width: Some(1920),
                height: Some(1080),
            },
        );

        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0].codec, VideoCodec::Vp9);
        assert_eq!(formats[0].profile, VideoProfile::Vp9(Vp9Profile::Profile0));
        assert_eq!(formats[0].bit_depth, BitDepth::Eight);
        assert_eq!(formats[0].chroma, ChromaSubsampling::Yuv420);
        assert!(p010_storage_layouts_for_formats(&formats).is_empty());
    }

    #[test]
    fn vp9_profile2_yuv420_10_reports_baseline_p010_layout() {
        let formats = formats_for_va_profile(
            libva::VAProfile::VAProfileVP9Profile2,
            libva::VA_RT_FORMAT_YUV420_10,
            MaxResolution {
                width: Some(3840),
                height: Some(2160),
            },
        );

        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0].codec, VideoCodec::Vp9);
        assert_eq!(formats[0].profile, VideoProfile::Vp9(Vp9Profile::Profile2));
        assert_eq!(formats[0].bit_depth, BitDepth::Ten);
        assert_eq!(formats[0].chroma, ChromaSubsampling::Yuv420);
        assert_eq!(
            p010_storage_layouts_for_formats(&formats),
            vec![DmaBufImageLayout::SeparateLayers]
        );
    }

    #[test]
    fn capability_probe_advertises_implemented_h264_8bit_yuv420_slot() {
        let formats = formats_for_va_profile(
            libva::VAProfile::VAProfileH264High,
            libva::VA_RT_FORMAT_YUV420 | libva::VA_RT_FORMAT_YUV420_10,
            MaxResolution {
                width: Some(1920),
                height: Some(1080),
            },
        );

        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0].codec, VideoCodec::H264);
        assert_eq!(formats[0].profile, VideoProfile::H264(H264Profile::High));
        assert_eq!(formats[0].bit_depth, BitDepth::Eight);
        assert_eq!(formats[0].chroma, ChromaSubsampling::Yuv420);
    }

    #[test]
    fn capability_probe_advertises_h265_main_and_main10_yuv420_slots() {
        let max_resolution = MaxResolution {
            width: Some(3840),
            height: Some(2160),
        };
        let main_formats = formats_for_va_profile(
            libva::VAProfile::VAProfileHEVCMain,
            libva::VA_RT_FORMAT_YUV420 | libva::VA_RT_FORMAT_YUV420_10,
            max_resolution,
        );
        let main10_formats = formats_for_va_profile(
            libva::VAProfile::VAProfileHEVCMain10,
            libva::VA_RT_FORMAT_YUV420 | libva::VA_RT_FORMAT_YUV420_10,
            max_resolution,
        );

        assert_eq!(main_formats.len(), 1);
        assert_eq!(main_formats[0].codec, VideoCodec::H265);
        assert_eq!(
            main_formats[0].profile,
            VideoProfile::H265(H265Profile::Main)
        );
        assert_eq!(main_formats[0].bit_depth, BitDepth::Eight);
        assert_eq!(main_formats[0].chroma, ChromaSubsampling::Yuv420);
        assert!(!main_formats[0].hdr_input);
        assert_eq!(main10_formats.len(), 1);
        assert_eq!(main10_formats[0].codec, VideoCodec::H265);
        assert_eq!(
            main10_formats[0].profile,
            VideoProfile::H265(H265Profile::Main10)
        );
        assert_eq!(main10_formats[0].bit_depth, BitDepth::Ten);
        assert_eq!(main10_formats[0].chroma, ChromaSubsampling::Yuv420);
        assert!(main10_formats[0].hdr_input);
    }

    #[test]
    fn capability_probe_does_not_advertise_unimplemented_vp9_profiles() {
        let profile1_formats = formats_for_va_profile(
            libva::VAProfile::VAProfileVP9Profile1,
            libva::VA_RT_FORMAT_YUV422,
            MaxResolution {
                width: Some(1920),
                height: Some(1080),
            },
        );
        let profile3_formats = formats_for_va_profile(
            libva::VAProfile::VAProfileVP9Profile3,
            libva::VA_RT_FORMAT_YUV420_10,
            MaxResolution {
                width: Some(1920),
                height: Some(1080),
            },
        );

        assert!(profile1_formats.is_empty());
        assert!(profile3_formats.is_empty());
    }

    #[test]
    fn capability_probe_does_not_advertise_future_codecs_without_adapters() {
        for profile in [
            libva::VAProfile::VAProfileAV1Profile0,
            libva::VAProfile::VAProfileVP8Version0_3,
        ] {
            let formats = formats_for_va_profile(
                profile,
                libva::VA_RT_FORMAT_YUV420,
                MaxResolution {
                    width: Some(1920),
                    height: Some(1080),
                },
            );

            assert!(formats.is_empty(), "{profile:?} must not be advertised");
        }
    }

    #[test]
    fn capability_probe_does_not_advertise_future_hevc_profiles() {
        for (profile, rt_format) in [
            (
                libva::VAProfile::VAProfileHEVCMain12,
                libva::VA_RT_FORMAT_YUV420_12,
            ),
            (
                libva::VAProfile::VAProfileHEVCMain422_10,
                libva::VA_RT_FORMAT_YUV422_10,
            ),
            (
                libva::VAProfile::VAProfileHEVCMain444,
                libva::VA_RT_FORMAT_YUV444,
            ),
            (
                libva::VAProfile::VAProfileHEVCSccMain,
                libva::VA_RT_FORMAT_YUV420,
            ),
        ] {
            let formats = formats_for_va_profile(
                profile,
                rt_format,
                MaxResolution {
                    width: Some(1920),
                    height: Some(1080),
                },
            );

            assert!(formats.is_empty(), "{profile:?} must not be advertised");
        }
    }
}
