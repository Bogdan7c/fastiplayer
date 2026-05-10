use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoCodec,
    VideoDecodeRequirement, VideoProfile, Vp9Profile,
};
use render_core::{P010RenderReadiness, RenderCapabilities, VideoFrameFormat};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{BackendCapabilities, SystemCapabilities, VideoExportPath};

/// Candidate video stream до выбора source/player layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct VideoStreamCandidate {
    /// Stable stream id из container/service manifest.
    pub stream_id: String,

    /// Требования потока к hardware decoder-у.
    pub requirement: VideoDecodeRequirement,

    /// Чем больше значение, тем предпочтительнее поток при равной поддержке.
    pub quality_score: i64,
}

/// Выбранный поток вместе с backend format, который его поддержал.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SelectedVideoStream {
    /// Stable stream id выбранного candidate-а.
    pub stream_id: String,

    /// Требования выбранного потока.
    pub requirement: VideoDecodeRequirement,

    /// Decode format/backend, который удовлетворил requirement.
    pub matched_format: SupportedVideoDecodeFormat,
}

/// Ошибка выбора stream-а.
#[derive(Debug, Clone, PartialEq, Error, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamSelectionError {
    /// Список candidate streams пуст.
    #[error("video stream candidates are empty")]
    EmptyCandidates,

    /// Ни один candidate не поддерживается hardware backend-ами.
    #[error("{0}")]
    Unsupported(UnsupportedVideoRequirement),
}

/// Подробное объяснение, почему stream нельзя декодировать.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UnsupportedVideoRequirement {
    /// Требования проблемного stream-а.
    pub requirement: VideoDecodeRequirement,

    /// Причины отказа в порядке полезности для UI.
    pub rejections: Vec<VideoCapabilityRejection>,

    /// Краткая сводка поддерживаемых форматов.
    pub supported_formats_summary: String,
}

impl UnsupportedVideoRequirement {
    /// Формирует user-facing ошибку на русском.
    #[must_use]
    pub fn user_message(&self) -> String {
        let reason = self
            .rejections
            .first()
            .map(VideoCapabilityRejection::user_message)
            .unwrap_or_else(|| "не найден подходящий аппаратный decoder".to_string());

        format!(
            "Не найден аппаратно поддерживаемый видеопоток.\nПричина: {reason}.\nВидео требует: {}.\nСистема поддерживает: {}.\nSoftware fallback для видео отключен политикой rustiplayer.",
            self.requirement.describe(),
            self.supported_formats_summary
        )
    }
}

impl std::fmt::Display for UnsupportedVideoRequirement {
    /// Печатает user-facing сообщение, чтобы ошибка была полезной в UI.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.user_message())
    }
}

/// Одна причина отказа capability matching.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCapabilityRejection {
    /// Нет доступных video backend-ов.
    NoAvailableBackend,

    /// Нет renderer backend-а, поэтому production playback не может пройти intersection.
    NoAvailableRenderer,

    /// Codec отсутствует во всей matrix.
    UnsupportedCodec {
        /// Запрошенный codec.
        codec: VideoCodec,
    },

    /// Codec есть, но нужного profile нет.
    UnsupportedProfile {
        /// Codec, для которого проверялся profile.
        codec: VideoCodec,

        /// Запрошенный profile.
        profile: VideoProfile,
    },

    /// Bit depth не входит в текущий production decode/render contract.
    UnsupportedBitDepth {
        /// Codec, для которого проверялся bit depth.
        codec: VideoCodec,

        /// Запрошенная bit depth.
        bit_depth: BitDepth,
    },

    /// Chroma subsampling не входит в текущий production render contract.
    UnsupportedChroma {
        /// Codec, для которого проверялась chroma.
        codec: VideoCodec,

        /// Запрошенная chroma subsampling.
        chroma: ChromaSubsampling,
    },

    /// Decode/backend matrix не содержит точного совпадения для формата stream-а.
    UnsupportedDecodeFormat {
        /// Codec stream-а.
        codec: VideoCodec,
    },

    /// Decode backend есть, но нужный device/export path не доступен.
    UnsupportedDeviceExportPath {
        /// Backend, который мог декодировать stream.
        backend: DecodeBackendId,

        /// Формат кадра на границе decoder/renderer.
        frame_format: VideoFrameFormat,

        /// Export path, обязательный для этого frame format.
        required_export_path: VideoExportPath,
    },

    /// Renderer не умеет принять нужный decoded frame format.
    UnsupportedRenderFrameFormat {
        /// Формат кадра на входе renderer-а.
        frame_format: VideoFrameFormat,
    },

    /// P010 boundary может быть диагностически проверен, но production render path ещё не готов.
    P010NotRenderable {
        /// Текущий readiness state renderer-а.
        readiness: P010RenderReadiness,
    },

    /// Decode возможен, но HDR renderer/tone mapper отсутствует.
    UnsupportedHdrRenderer {
        /// Формат кадра, который пришёл бы на renderer boundary.
        frame_format: Option<VideoFrameFormat>,
    },

    /// Требование слишком неполное для безопасного production selection.
    InsufficientStreamMetadata {
        /// Codec stream-а.
        codec: VideoCodec,
    },
}

impl VideoCapabilityRejection {
    /// Формирует короткий текст причины для UI.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::NoAvailableBackend => "hardware video backend недоступен".to_string(),
            Self::NoAvailableRenderer => "renderer backend недоступен".to_string(),
            Self::UnsupportedCodec { codec } => format!("codec {codec} не поддерживается"),
            Self::UnsupportedProfile { codec: _, profile } => {
                format!("profile {profile} не поддерживается аппаратным backend-ом")
            }
            Self::UnsupportedBitDepth { codec, bit_depth } => format!(
                "{codec} {bit_depth} не поддерживается: для этой bit depth нет production decode/render path"
            ),
            Self::UnsupportedChroma { codec, chroma } => format!(
                "{codec} chroma {chroma} не поддерживается: production path принимает только 4:2:0"
            ),
            Self::UnsupportedDecodeFormat { codec } => format!(
                "для codec {codec} нет decode format, совпадающего с profile/bit depth/chroma/resolution stream-а"
            ),
            Self::UnsupportedDeviceExportPath {
                backend,
                frame_format,
                required_export_path,
            } => format!(
                "backend {backend} может декодировать {frame_format}, но не подтвердил обязательный export path {required_export_path}"
            ),
            Self::UnsupportedRenderFrameFormat { frame_format } => {
                format!("renderer не поддерживает input format {frame_format}")
            }
            Self::P010NotRenderable { readiness } => format!(
                "P010 zero-copy boundary имеет состояние `{readiness}`, но production P010 renderer ещё недоступен"
            ),
            Self::UnsupportedHdrRenderer { frame_format } => {
                let frame_format = frame_format
                    .map(|format| format.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "HDR stream нельзя выбрать: HDR-to-SDR renderer для {frame_format} пока недоступен"
                )
            }
            Self::InsufficientStreamMetadata { codec } => format!(
                "metadata stream-а для codec {codec} недостаточно точна для безопасного выбора"
            ),
        }
    }
}

impl SystemCapabilities {
    /// Ищет первый backend format, который удовлетворяет stream requirement.
    #[must_use]
    pub fn find_supported_video_format(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> Option<&SupportedVideoDecodeFormat> {
        self.supported_video_formats()
            .find(|format| format.satisfies(requirement))
    }

    /// Ищет renderer backend, который сможет показать stream requirement.
    #[must_use]
    pub fn find_supported_render_capability(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> Option<&RenderCapabilities> {
        self.render_backends
            .iter()
            .find(|capabilities| capabilities.supports_decode_requirement(requirement))
    }

    /// Ищет backend report, которому принадлежит matched decode format.
    fn backend_for_decode_format(
        &self,
        format: &SupportedVideoDecodeFormat,
    ) -> Option<&BackendCapabilities> {
        self.video_backends.iter().find(|backend| {
            backend.status.is_available()
                && backend.backend_id == format.backend
                && backend
                    .supported_video_decode_formats
                    .iter()
                    .any(|candidate| candidate == format)
        })
    }

    /// Проверяет одно stream requirement и возвращает detailed error при отказе.
    pub fn check_video_requirement(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> Result<&SupportedVideoDecodeFormat, UnsupportedVideoRequirement> {
        let frame_format = match frame_format_for_requirement(requirement) {
            Ok(frame_format) => frame_format,
            Err(rejection) => {
                return Err(
                    self.unsupported_requirement_with_rejections(requirement, vec![rejection])
                );
            }
        };

        let Some(format) = self.find_supported_video_format(requirement) else {
            return Err(self.explain_unsupported_video_requirement(requirement));
        };

        let Some(backend) = self.backend_for_decode_format(format) else {
            return Err(self.unsupported_requirement_with_rejections(
                requirement,
                vec![VideoCapabilityRejection::UnsupportedDecodeFormat {
                    codec: requirement.codec,
                }],
            ));
        };

        if let Some(rejection) = device_boundary_rejection(frame_format, backend) {
            return Err(self.unsupported_requirement_with_rejections(requirement, vec![rejection]));
        }

        if let Some(rejection) = self.render_rejection(requirement, frame_format) {
            return Err(self.unsupported_requirement_with_rejections(requirement, vec![rejection]));
        }

        Ok(format)
    }

    /// Проверяет stream requirement для ручной Phase 9 P010 boundary diagnostics.
    ///
    /// Production `check_video_requirement()` обязан учитывать renderer и HDR-to-SDR
    /// readiness. Этот метод намеренно проверяет только то, что нужно до renderer-а:
    /// hardware decode format и обязательный DMA-BUF export path для P010.
    pub fn check_video_requirement_for_p010_boundary_diagnostic(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> Result<&SupportedVideoDecodeFormat, UnsupportedVideoRequirement> {
        let frame_format = match frame_format_for_requirement(requirement) {
            Ok(frame_format) => frame_format,
            Err(rejection) => {
                return Err(
                    self.unsupported_requirement_with_rejections(requirement, vec![rejection])
                );
            }
        };

        if frame_format != VideoFrameFormat::P010 {
            if !can_probe_p010_boundary_from_incomplete_hdr(requirement) {
                return self.check_video_requirement(requirement);
            }

            let Some(format) = self.find_p010_boundary_diagnostic_format(requirement) else {
                return Err(self.explain_unsupported_video_requirement(requirement));
            };

            let Some(backend) = self.backend_for_decode_format(format) else {
                return Err(self.unsupported_requirement_with_rejections(
                    requirement,
                    vec![VideoCapabilityRejection::UnsupportedDecodeFormat {
                        codec: requirement.codec,
                    }],
                ));
            };

            if let Some(rejection) = device_boundary_rejection(VideoFrameFormat::P010, backend) {
                return Err(
                    self.unsupported_requirement_with_rejections(requirement, vec![rejection])
                );
            }

            return Ok(format);
        }

        let Some(format) = self.find_supported_video_format(requirement) else {
            return Err(self.explain_unsupported_video_requirement(requirement));
        };

        let Some(backend) = self.backend_for_decode_format(format) else {
            return Err(self.unsupported_requirement_with_rejections(
                requirement,
                vec![VideoCapabilityRejection::UnsupportedDecodeFormat {
                    codec: requirement.codec,
                }],
            ));
        };

        if let Some(rejection) = device_boundary_rejection(frame_format, backend) {
            return Err(self.unsupported_requirement_with_rejections(requirement, vec![rejection]));
        }

        Ok(format)
    }

    /// Ищет hardware format, который может довести неполный VP9 HDR requirement до P010 probe.
    fn find_p010_boundary_diagnostic_format(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> Option<&SupportedVideoDecodeFormat> {
        self.supported_video_formats().find(|format| {
            format.satisfies(requirement)
                && format.bit_depth == BitDepth::Ten
                && format.chroma == ChromaSubsampling::Yuv420
                && matches!(format.profile, VideoProfile::Vp9(Vp9Profile::Profile2))
        })
    }

    /// Выбирает лучший поддерживаемый stream из candidates.
    pub fn select_best_video_stream(
        &self,
        candidates: &[VideoStreamCandidate],
    ) -> Result<SelectedVideoStream, StreamSelectionError> {
        if candidates.is_empty() {
            return Err(StreamSelectionError::EmptyCandidates);
        }

        let mut ordered_candidates = candidates.iter().collect::<Vec<_>>();
        ordered_candidates.sort_by(|left, right| {
            right
                .quality_score
                .cmp(&left.quality_score)
                .then_with(|| left.stream_id.cmp(&right.stream_id))
        });

        let mut first_rejection = None;
        for candidate in ordered_candidates {
            match self.check_video_requirement(&candidate.requirement) {
                Ok(format) => {
                    return Ok(SelectedVideoStream {
                        stream_id: candidate.stream_id.clone(),
                        requirement: candidate.requirement.clone(),
                        matched_format: format.clone(),
                    });
                }
                Err(error) => {
                    if first_rejection.is_none() {
                        first_rejection = Some(error);
                    }
                }
            }
        }

        Err(StreamSelectionError::Unsupported(
            first_rejection.unwrap_or_else(|| {
                self.explain_unsupported_video_requirement(&candidates[0].requirement)
            }),
        ))
    }

    /// Строит structured rejection list без потери исходного requirement.
    #[must_use]
    pub fn explain_unsupported_video_requirement(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> UnsupportedVideoRequirement {
        let supported_formats = self.supported_video_formats().collect::<Vec<_>>();
        if let Err(rejection) = frame_format_for_requirement(requirement) {
            return self.unsupported_requirement_with_rejections(requirement, vec![rejection]);
        }

        let decode_match = supported_formats
            .iter()
            .any(|format| format.satisfies(requirement));
        let rejections = if supported_formats.is_empty() {
            vec![VideoCapabilityRejection::NoAvailableBackend]
        } else if !supported_formats
            .iter()
            .any(|format| format.codec == requirement.codec)
        {
            vec![VideoCapabilityRejection::UnsupportedCodec {
                codec: requirement.codec,
            }]
        } else if let Some(profile) = requirement.profile
            && !supported_formats
                .iter()
                .any(|format| format.codec == requirement.codec && format.profile == profile)
        {
            vec![VideoCapabilityRejection::UnsupportedProfile {
                codec: requirement.codec,
                profile,
            }]
        } else if let Some(bit_depth) = requirement.bit_depth
            && !supported_formats
                .iter()
                .any(|format| format.codec == requirement.codec && format.bit_depth == bit_depth)
        {
            vec![VideoCapabilityRejection::UnsupportedBitDepth {
                codec: requirement.codec,
                bit_depth,
            }]
        } else if let Some(chroma) = requirement.chroma
            && !supported_formats
                .iter()
                .any(|format| format.codec == requirement.codec && format.chroma == chroma)
        {
            vec![VideoCapabilityRejection::UnsupportedChroma {
                codec: requirement.codec,
                chroma,
            }]
        } else if decode_match {
            let frame_format = frame_format_for_requirement(requirement).ok();
            let device_rejection = frame_format.and_then(|frame_format| {
                self.find_supported_video_format(requirement)
                    .and_then(|format| self.backend_for_decode_format(format))
                    .and_then(|backend| device_boundary_rejection(frame_format, backend))
            });
            device_rejection
                .or_else(|| {
                    frame_format
                        .and_then(|frame_format| self.render_rejection(requirement, frame_format))
                })
                .map(|rejection| vec![rejection])
                .unwrap_or_else(|| {
                    vec![VideoCapabilityRejection::UnsupportedDecodeFormat {
                        codec: requirement.codec,
                    }]
                })
        } else if requirement.profile.is_some() {
            vec![VideoCapabilityRejection::UnsupportedDecodeFormat {
                codec: requirement.codec,
            }]
        } else {
            vec![VideoCapabilityRejection::InsufficientStreamMetadata {
                codec: requirement.codec,
            }]
        };

        self.unsupported_requirement_with_rejections(requirement, rejections)
    }

    /// Собирает typed unsupported result с общей сводкой возможностей системы.
    fn unsupported_requirement_with_rejections(
        &self,
        requirement: &VideoDecodeRequirement,
        rejections: Vec<VideoCapabilityRejection>,
    ) -> UnsupportedVideoRequirement {
        let supported_formats = self.supported_video_formats().collect::<Vec<_>>();

        UnsupportedVideoRequirement {
            requirement: requirement.clone(),
            rejections,
            supported_formats_summary: summarize_system_support(
                &supported_formats,
                &self.render_backends,
            ),
        }
    }

    /// Возвращает renderer-side reject после успешного decode/device match.
    fn render_rejection(
        &self,
        requirement: &VideoDecodeRequirement,
        frame_format: VideoFrameFormat,
    ) -> Option<VideoCapabilityRejection> {
        if self.render_backends.is_empty() {
            return Some(VideoCapabilityRejection::NoAvailableRenderer);
        }

        if self.find_supported_render_capability(requirement).is_some() {
            return None;
        }

        if requirement.hdr
            && !self.render_backends.iter().any(|capabilities| {
                capabilities.supports_hdr_to_sdr || capabilities.supports_native_hdr_output
            })
        {
            return Some(VideoCapabilityRejection::UnsupportedHdrRenderer {
                frame_format: Some(frame_format),
            });
        }

        if frame_format == VideoFrameFormat::P010 {
            let readiness = best_p010_render_readiness(&self.render_backends);
            if !readiness.is_renderable() {
                return Some(VideoCapabilityRejection::P010NotRenderable { readiness });
            }
        }

        if !self
            .render_backends
            .iter()
            .any(|capabilities| capabilities.supports_frame_format(frame_format))
        {
            return Some(VideoCapabilityRejection::UnsupportedRenderFrameFormat { frame_format });
        }

        Some(VideoCapabilityRejection::UnsupportedDecodeFormat {
            codec: requirement.codec,
        })
    }
}

/// Формирует компактный список decode/render возможностей для ошибки.
fn summarize_system_support(
    supported_formats: &[&SupportedVideoDecodeFormat],
    render_backends: &[RenderCapabilities],
) -> String {
    let decode_summary = summarize_supported_formats(supported_formats);

    format!(
        "decode: {decode_summary}; render: {}",
        summarize_render_capabilities(render_backends)
    )
}

/// Формирует компактный список поддерживаемых decode форматов для ошибки.
fn summarize_supported_formats(supported_formats: &[&SupportedVideoDecodeFormat]) -> String {
    if supported_formats.is_empty() {
        return "нет доступных аппаратных video formats".to_string();
    }

    let mut descriptions = supported_formats
        .iter()
        .take(6)
        .map(|format| format.describe())
        .collect::<Vec<_>>();

    if supported_formats.len() > descriptions.len() {
        descriptions.push(format!(
            "ещё {} formats",
            supported_formats.len() - descriptions.len()
        ));
    }

    descriptions.join("; ")
}

/// Формирует компактный список renderer capabilities.
fn summarize_render_capabilities(render_backends: &[RenderCapabilities]) -> String {
    if render_backends.is_empty() {
        return "renderer capabilities не зарегистрированы".to_string();
    }

    render_backends
        .iter()
        .map(RenderCapabilities::summary_text)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Выводит renderer input format или typed policy reject для неподдержанных вариантов.
fn frame_format_for_requirement(
    requirement: &VideoDecodeRequirement,
) -> Result<VideoFrameFormat, VideoCapabilityRejection> {
    if let Some(chroma) = requirement.chroma
        && chroma != ChromaSubsampling::Yuv420
    {
        return Err(VideoCapabilityRejection::UnsupportedChroma {
            codec: requirement.codec,
            chroma,
        });
    }

    if let Some(bit_depth) = requirement.bit_depth
        && bit_depth == BitDepth::Twelve
    {
        return Err(VideoCapabilityRejection::UnsupportedBitDepth {
            codec: requirement.codec,
            bit_depth,
        });
    }

    VideoFrameFormat::from_decode_requirement(requirement).ok_or(
        VideoCapabilityRejection::InsufficientStreamMetadata {
            codec: requirement.codec,
        },
    )
}

/// Разрешает Phase 9 dev-mode попробовать VP9 bitstream probe до production renderer gate.
fn can_probe_p010_boundary_from_incomplete_hdr(requirement: &VideoDecodeRequirement) -> bool {
    requirement.codec == VideoCodec::Vp9
        && requirement.hdr
        && requirement
            .profile
            .is_none_or(|profile| matches!(profile, VideoProfile::Vp9(Vp9Profile::Profile2)))
        && requirement
            .bit_depth
            .is_none_or(|bit_depth| bit_depth == BitDepth::Ten)
        && requirement
            .chroma
            .is_none_or(|chroma| chroma == ChromaSubsampling::Yuv420)
}

/// Проверяет device/export часть intersection для decoded frame format-а.
fn device_boundary_rejection(
    frame_format: VideoFrameFormat,
    backend: &BackendCapabilities,
) -> Option<VideoCapabilityRejection> {
    if frame_format != VideoFrameFormat::P010 {
        return None;
    }

    if backend.export_paths.contains(&VideoExportPath::DmaBuf) {
        return None;
    }

    Some(VideoCapabilityRejection::UnsupportedDeviceExportPath {
        backend: backend.backend_id.clone(),
        frame_format,
        required_export_path: VideoExportPath::DmaBuf,
    })
}

/// Возвращает самый сильный P010 readiness state среди renderer backend-ов.
fn best_p010_render_readiness(render_backends: &[RenderCapabilities]) -> P010RenderReadiness {
    render_backends
        .iter()
        .map(|capabilities| capabilities.p010_render_readiness)
        .max()
        .unwrap_or(P010RenderReadiness::Unavailable)
}

#[cfg(test)]
mod tests {
    use codec_core::{
        BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, DecodeBackendId,
        MatrixCoefficients, SupportedVideoDecodeFormat, TransferFunction, VideoCodec,
        VideoColorMetadata, VideoDecodeRequirement, VideoProfile, Vp9Profile,
    };
    use render_core::{P010RenderReadiness, RenderCapabilities};

    use crate::{BackendCapabilities, BackendDriverInfo, BackendProbeStatus, SystemCapabilities};

    use super::*;

    fn capabilities_with_vp9_profile0() -> SystemCapabilities {
        capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile0,
                BitDepth::Eight,
                ChromaSubsampling::Yuv420,
                false,
            )],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
            vec![VideoExportPath::DmaBuf],
        )
    }

    fn capabilities_with_formats(
        supported_formats: Vec<SupportedVideoDecodeFormat>,
        render_backends: Vec<RenderCapabilities>,
        export_paths: Vec<VideoExportPath>,
    ) -> SystemCapabilities {
        SystemCapabilities {
            schema_version: crate::CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![BackendCapabilities {
                backend_id: DecodeBackendId::vaapi(),
                display_name: "VA-API".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                supported_video_decode_formats: supported_formats,
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                export_paths,
                diagnostics: Vec::new(),
            }],
            render_backends,
        }
    }

    fn vp9_format(
        profile: Vp9Profile,
        bit_depth: BitDepth,
        chroma: ChromaSubsampling,
        hdr_input: bool,
    ) -> SupportedVideoDecodeFormat {
        SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: VideoProfile::Vp9(profile),
            bit_depth,
            chroma,
            max_width: Some(4096),
            max_height: Some(2304),
            max_fps: None,
            hdr_input,
            backend: DecodeBackendId::vaapi(),
        }
    }

    fn vp9_requirement(
        profile: Vp9Profile,
        bit_depth: BitDepth,
        chroma: ChromaSubsampling,
    ) -> VideoDecodeRequirement {
        VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(profile))
            .with_bit_depth(bit_depth)
            .with_chroma(chroma)
    }

    fn bt2020_pq_limited() -> VideoColorMetadata {
        VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Pq,
            None,
        )
    }

    #[test]
    fn selection_picks_supported_highest_quality_candidate() {
        let capabilities = capabilities_with_vp9_profile0();
        let candidates = vec![
            VideoStreamCandidate {
                stream_id: "low".to_string(),
                requirement: VideoDecodeRequirement::new(VideoCodec::Vp9)
                    .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0)),
                quality_score: 10,
            },
            VideoStreamCandidate {
                stream_id: "high".to_string(),
                requirement: VideoDecodeRequirement::new(VideoCodec::Vp9)
                    .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0)),
                quality_score: 20,
            },
        ];

        let selected = capabilities
            .select_best_video_stream(&candidates)
            .expect("supported stream should be selected");

        assert_eq!(selected.stream_id, "high");
    }

    #[test]
    fn unsupported_profile_is_reported_before_decode() {
        let capabilities = capabilities_with_vp9_profile0();
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2));

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("profile2 should be unsupported");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedProfile { .. })
        ));
        assert!(error.user_message().contains("profile VP9 Profile 2"));
    }

    #[test]
    fn profile1_and_profile3_are_rejected_as_unsupported_chroma() {
        let cases = [
            (
                Vp9Profile::Profile1,
                BitDepth::Eight,
                ChromaSubsampling::Yuv422,
            ),
            (
                Vp9Profile::Profile3,
                BitDepth::Ten,
                ChromaSubsampling::Yuv444,
            ),
        ];

        for (profile, bit_depth, chroma) in cases {
            let capabilities = capabilities_with_formats(
                vec![vp9_format(profile, bit_depth, chroma, false)],
                vec![RenderCapabilities::wgpu_nv12(Some(4096))],
                vec![VideoExportPath::DmaBuf],
            );
            let requirement = vp9_requirement(profile, bit_depth, chroma);

            let error = capabilities
                .check_video_requirement(&requirement)
                .expect_err("VP9 non-4:2:0 profiles must be rejected by chroma policy");

            assert!(matches!(
                error.rejections.first(),
                Some(VideoCapabilityRejection::UnsupportedChroma {
                    codec: VideoCodec::Vp9,
                    chroma: rejected_chroma,
                }) if *rejected_chroma == chroma
            ));
        }
    }

    #[test]
    fn twelve_bit_requirement_is_rejected_as_unsupported_bit_depth() {
        let capabilities = capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Twelve,
                ChromaSubsampling::Yuv420,
                true,
            )],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
            vec![VideoExportPath::DmaBuf],
        );
        let requirement = vp9_requirement(
            Vp9Profile::Profile2,
            BitDepth::Twelve,
            ChromaSubsampling::Yuv420,
        );

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("12-bit stream must be rejected before render selection");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedBitDepth {
                codec: VideoCodec::Vp9,
                bit_depth: BitDepth::Twelve,
            })
        ));
    }

    #[test]
    fn vp9_profile2_10bit_hdr_is_rejected_until_hdr_renderer_exists() {
        let capabilities = capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            )],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
            vec![VideoExportPath::DmaBuf],
        );
        let requirement = vp9_requirement(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
        )
        .with_color(bt2020_pq_limited());

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("HDR stream must wait for Phase 10 HDR renderer");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedHdrRenderer {
                frame_format: Some(VideoFrameFormat::P010),
            })
        ));
        assert!(error.user_message().contains("HDR-to-SDR renderer"));
    }

    #[test]
    fn p010_boundary_diagnostic_allows_decode_without_hdr_renderer() {
        let capabilities = capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            )],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
            vec![VideoExportPath::DmaBuf],
        );
        let requirement = vp9_requirement(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
        )
        .with_color(bt2020_pq_limited());

        capabilities
            .check_video_requirement(&requirement)
            .expect_err("production HDR playback must remain rejected before Phase 10");

        let selected_format = capabilities
            .check_video_requirement_for_p010_boundary_diagnostic(&requirement)
            .expect("Phase 9 diagnostic path should allow decode/P010 boundary verification");

        assert_eq!(selected_format.bit_depth, BitDepth::Ten);
        assert_eq!(selected_format.chroma, ChromaSubsampling::Yuv420);
    }

    #[test]
    fn p010_boundary_diagnostic_allows_incomplete_vp9_hdr_track_metadata() {
        let capabilities = capabilities_with_formats(
            vec![
                vp9_format(
                    Vp9Profile::Profile0,
                    BitDepth::Eight,
                    ChromaSubsampling::Yuv420,
                    false,
                ),
                vp9_format(
                    Vp9Profile::Profile2,
                    BitDepth::Ten,
                    ChromaSubsampling::Yuv420,
                    true,
                ),
            ],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
            vec![VideoExportPath::DmaBuf],
        );
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_resolution(3840, 2160)
            .with_color(bt2020_pq_limited());

        let production_error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("production selection must still reject HDR before Phase 10");
        assert!(matches!(
            production_error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedHdrRenderer {
                frame_format: Some(VideoFrameFormat::Nv12),
            })
        ));

        let selected_format = capabilities
            .check_video_requirement_for_p010_boundary_diagnostic(&requirement)
            .expect("Phase 9 diagnostic mode should let packet probe refine VP9 HDR to P010");

        assert_eq!(
            selected_format.profile,
            VideoProfile::Vp9(Vp9Profile::Profile2)
        );
        assert_eq!(selected_format.bit_depth, BitDepth::Ten);
        assert!(selected_format.hdr_input);
    }

    #[test]
    fn p010_boundary_diagnostic_does_not_bypass_explicit_8bit_hdr_requirement() {
        let capabilities = capabilities_with_formats(
            vec![
                vp9_format(
                    Vp9Profile::Profile0,
                    BitDepth::Eight,
                    ChromaSubsampling::Yuv420,
                    false,
                ),
                vp9_format(
                    Vp9Profile::Profile2,
                    BitDepth::Ten,
                    ChromaSubsampling::Yuv420,
                    true,
                ),
            ],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
            vec![VideoExportPath::DmaBuf],
        );
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(BitDepth::Eight)
            .with_color(bt2020_pq_limited());

        let error = capabilities
            .check_video_requirement_for_p010_boundary_diagnostic(&requirement)
            .expect_err("explicit NV12/HDR requirement must remain gated by HDR renderer");

        assert!(!matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedDeviceExportPath {
                frame_format: VideoFrameFormat::P010,
                ..
            })
        ));
        assert!(
            !can_probe_p010_boundary_from_incomplete_hdr(&requirement),
            "explicit 8-bit HDR must not enter the P010 diagnostic probe branch"
        );
    }

    #[test]
    fn p010_boundary_diagnostic_requires_dma_buf_export_path() {
        let capabilities = capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            )],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
            Vec::new(),
        );
        let requirement = vp9_requirement(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
        )
        .with_color(bt2020_pq_limited());

        let error = capabilities
            .check_video_requirement_for_p010_boundary_diagnostic(&requirement)
            .expect_err("P010 boundary diagnostics must require DMA-BUF export");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedDeviceExportPath {
                frame_format: VideoFrameFormat::P010,
                required_export_path: VideoExportPath::DmaBuf,
                ..
            })
        ));
    }

    #[test]
    fn p010_boundary_verified_state_alone_does_not_make_stream_playable() {
        let mut render_capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
        render_capabilities.p010_render_readiness = P010RenderReadiness::ZeroCopyBoundaryVerified;
        let capabilities = capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                false,
            )],
            vec![render_capabilities],
            vec![VideoExportPath::DmaBuf],
        );
        let requirement = vp9_requirement(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
        );

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("P010 boundary diagnostics must not enable production playback");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::P010NotRenderable {
                readiness: P010RenderReadiness::ZeroCopyBoundaryVerified,
            })
        ));
    }

    #[test]
    fn reason_formatter_produces_user_facing_russian_explanation() {
        let message = VideoCapabilityRejection::UnsupportedBitDepth {
            codec: VideoCodec::Vp9,
            bit_depth: BitDepth::Twelve,
        }
        .user_message();

        assert!(message.contains("VP9 12-bit"));
        assert!(message.contains("не поддерживается"));
    }
}
