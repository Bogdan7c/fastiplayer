use codec_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, DecodeBackendId, MatrixCoefficients,
    SupportedVideoDecodeFormat, TransferFunction, VideoCodec, VideoDecodeRequirement, VideoProfile,
    video_frame_pixel_layout_from_decode_requirement,
};
use render_core::{
    P010RenderReadiness, RenderCapabilities, RenderFrameContractRejection,
    RenderVideoOutputRejection,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use video_frame_contract::{
    DmaBufImageLayout, HardwareFrameHandle, VideoFrameContract, VideoFramePixelLayout,
    VideoFrameTransferPath,
};

use crate::{SupportedVideoOutput, SystemCapabilities};

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

/// Выбранный поток вместе с backend output, который прошёл renderer intersection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SelectedVideoStream {
    /// Stable stream id выбранного candidate-а.
    pub stream_id: String,

    /// Требования выбранного потока.
    pub requirement: VideoDecodeRequirement,

    /// Concrete output/backend, который удовлетворил requirement.
    pub matched_output: SupportedVideoOutput,
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
    pub requirement: Box<VideoDecodeRequirement>,

    /// Причины отказа в порядке полезности для UI.
    pub rejections: Vec<VideoCapabilityRejection>,

    /// Краткая сводка поддерживаемых форматов.
    pub supported_formats_summary: Box<str>,
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
            "Не найден аппаратно поддерживаемый видеопоток.\nПричина: {reason}.\nВидео требует: {}.\nСистема поддерживает: {}.\nSoftware fallback для видео отключен политикой fastiplayer.",
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

    /// Decode backend есть, но не объявил renderer-compatible transfer/layout.
    UnsupportedBackendFrameTransfer {
        /// Backend, который мог декодировать stream.
        backend: DecodeBackendId,

        /// Frame contract, который был нужен для renderer intersection.
        required_frame_contract: VideoFrameContract,
    },

    /// Renderer не умеет принять нужный decoded frame format.
    UnsupportedRenderFrameFormat {
        /// Формат кадра на входе renderer-а.
        frame_format: VideoFramePixelLayout,
    },

    /// Renderer не умеет принять нужный transfer path/layout для frame contract-а.
    UnsupportedRenderFrameTransfer {
        /// Полный frame contract, который renderer отклонил.
        frame_contract: VideoFrameContract,
    },

    /// Renderer texture limit меньше coded размера stream-а.
    RenderTextureSizeExceeded {
        /// Width stream-а, если он был известен.
        width: Option<u32>,

        /// Height stream-а, если он был известен.
        height: Option<u32>,

        /// Максимальный texture size renderer-а.
        max_texture_size: u32,
    },

    /// P010 boundary может быть диагностически проверен, но production render path ещё не готов.
    P010NotRenderable {
        /// Текущий readiness state renderer-а.
        readiness: P010RenderReadiness,
    },

    /// Renderer не включил feature, нужный для фактического P010 DMA-BUF layout-а.
    UnsupportedDmaBufImageLayout {
        /// Backend, который экспортирует P010 surface.
        backend: DecodeBackendId,

        /// Layout, который нужен для zero-copy import.
        storage_layout: DmaBufImageLayout,

        /// wgpu feature, без которого layout не считается production-ready.
        required_wgpu_feature: String,
    },

    /// Decode возможен, но HDR renderer/tone mapper отсутствует.
    UnsupportedHdrRenderer {
        /// Формат кадра, который пришёл бы на renderer boundary.
        frame_format: Option<VideoFramePixelLayout>,
    },

    /// HDR metadata не проходит strict core policy Phase 10.
    InvalidHdrMetadata {
        /// Конкретная причина отказа.
        reason: String,
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
            Self::NoAvailableBackend => "video decode backend недоступен".to_string(),
            Self::NoAvailableRenderer => "renderer backend недоступен".to_string(),
            Self::UnsupportedCodec { codec } => format!("codec {codec} не поддерживается"),
            Self::UnsupportedProfile { codec: _, profile } => {
                format!("profile {profile} не поддерживается доступными video decode backend-ами")
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
            Self::UnsupportedBackendFrameTransfer {
                backend,
                required_frame_contract,
            } => format!(
                "backend {backend} может декодировать stream, но не объявил output transfer/layout {}",
                required_frame_contract.diagnostic_label()
            ),
            Self::UnsupportedRenderFrameFormat { frame_format } => {
                format!("renderer не поддерживает input format {frame_format}")
            }
            Self::UnsupportedRenderFrameTransfer { frame_contract } => format!(
                "renderer не поддерживает transfer contract {}",
                frame_contract.diagnostic_label()
            ),
            Self::RenderTextureSizeExceeded {
                width,
                height,
                max_texture_size,
            } => {
                let width = width
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let height = height
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "renderer texture limit {max_texture_size} меньше coded размера stream-а {width}x{height}"
                )
            }
            Self::P010NotRenderable { readiness } => format!(
                "P010 zero-copy boundary имеет состояние `{readiness}`, но production P010 renderer ещё недоступен"
            ),
            Self::UnsupportedDmaBufImageLayout {
                backend,
                storage_layout,
                required_wgpu_feature,
            } => format!(
                "backend {backend} экспортирует P010 как {storage_layout}, но renderer не подтвердил required import feature {required_wgpu_feature}"
            ),
            Self::UnsupportedHdrRenderer { frame_format } => {
                let frame_format = frame_format
                    .map(|format| format.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "HDR stream нельзя выбрать: HDR-to-SDR renderer для {frame_format} пока недоступен"
                )
            }
            Self::InvalidHdrMetadata { reason } => {
                format!("HDR stream отклонён: {reason}")
            }
            Self::InsufficientStreamMetadata { codec } => format!(
                "metadata stream-а для codec {codec} недостаточно точна для безопасного выбора"
            ),
        }
    }
}

impl SystemCapabilities {
    /// Ищет первый playable output, который удовлетворяет stream requirement.
    #[must_use]
    pub fn find_supported_video_output(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> Option<&SupportedVideoOutput> {
        self.supported_video_outputs()
            .find(|output| output.satisfies(requirement))
    }

    /// Ищет playable output конкретного backend-а, который закрывает stream requirement.
    #[must_use]
    pub fn find_playable_video_output_for_backend(
        &self,
        backend_id: &DecodeBackendId,
        requirement: &VideoDecodeRequirement,
    ) -> Option<&SupportedVideoOutput> {
        self.supported_video_outputs()
            .find(|output| &output.backend == backend_id && output.satisfies(requirement))
    }

    /// Возвращает codec-level format выбранного playable output-а.
    #[must_use]
    pub fn find_supported_video_format(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> Option<&SupportedVideoDecodeFormat> {
        self.find_supported_video_output(requirement)
            .map(|output| &output.decode_format)
    }

    /// Возвращает raw provider outputs, которые закрывают codec-level stream requirement.
    fn matching_raw_video_outputs(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> Vec<&SupportedVideoOutput> {
        self.raw_video_outputs()
            .filter(|output| output.satisfies(requirement))
            .collect()
    }

    /// Проверяет одно stream requirement и возвращает detailed error при отказе.
    pub fn check_video_requirement(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> Result<&SupportedVideoOutput, UnsupportedVideoRequirement> {
        let frame_format = match frame_format_for_requirement(requirement) {
            Ok(frame_format) => frame_format,
            Err(rejection) => {
                return Err(
                    self.unsupported_requirement_with_rejections(requirement, vec![rejection])
                );
            }
        };

        let raw_outputs = self.matching_raw_video_outputs(requirement);
        if raw_outputs.is_empty() {
            return Err(self.explain_unsupported_video_requirement(requirement));
        }

        if let Some(rejection) = strict_hdr_metadata_rejection(requirement, frame_format) {
            return Err(self.unsupported_requirement_with_rejections(requirement, vec![rejection]));
        }

        if let Some(output) = self.find_supported_video_output(requirement) {
            return Ok(output);
        }

        if let Some(rejection) =
            self.output_intersection_rejection(requirement, frame_format, &raw_outputs)
        {
            return Err(self.unsupported_requirement_with_rejections(requirement, vec![rejection]));
        }

        Err(self.unsupported_requirement_with_rejections(
            requirement,
            vec![VideoCapabilityRejection::UnsupportedDecodeFormat {
                codec: requirement.codec,
            }],
        ))
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
                Ok(output) => {
                    return Ok(SelectedVideoStream {
                        stream_id: candidate.stream_id.clone(),
                        requirement: candidate.requirement.clone(),
                        matched_output: output.clone(),
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
        let raw_outputs = self.raw_video_outputs().collect::<Vec<_>>();
        if let Err(rejection) = frame_format_for_requirement(requirement) {
            return self.unsupported_requirement_with_rejections(requirement, vec![rejection]);
        }

        let decode_match = raw_outputs
            .iter()
            .any(|output| output.satisfies(requirement));
        let rejections = if raw_outputs.is_empty() {
            vec![VideoCapabilityRejection::NoAvailableBackend]
        } else if !raw_outputs
            .iter()
            .any(|output| output.decode_format.codec == requirement.codec)
        {
            vec![VideoCapabilityRejection::UnsupportedCodec {
                codec: requirement.codec,
            }]
        } else if let Some(profile) = requirement.profile
            && !raw_outputs.iter().any(|output| {
                output.decode_format.codec == requirement.codec
                    && output.decode_format.profile == profile
            })
        {
            vec![VideoCapabilityRejection::UnsupportedProfile {
                codec: requirement.codec,
                profile,
            }]
        } else if let Some(bit_depth) = requirement.bit_depth
            && !raw_outputs.iter().any(|output| {
                output.decode_format.codec == requirement.codec
                    && output.decode_format.bit_depth == bit_depth
            })
        {
            vec![VideoCapabilityRejection::UnsupportedBitDepth {
                codec: requirement.codec,
                bit_depth,
            }]
        } else if let Some(chroma) = requirement.chroma
            && !raw_outputs.iter().any(|output| {
                output.decode_format.codec == requirement.codec
                    && output.decode_format.chroma == chroma
            })
        {
            vec![VideoCapabilityRejection::UnsupportedChroma {
                codec: requirement.codec,
                chroma,
            }]
        } else if decode_match {
            let frame_format = frame_format_for_requirement(requirement).ok();
            if let Some(frame_format) = frame_format {
                strict_hdr_metadata_rejection(requirement, frame_format)
                    .or_else(|| {
                        let matching_raw_outputs = self.matching_raw_video_outputs(requirement);
                        self.output_intersection_rejection(
                            requirement,
                            frame_format,
                            &matching_raw_outputs,
                        )
                    })
                    .map(|rejection| vec![rejection])
                    .unwrap_or_else(|| {
                        vec![VideoCapabilityRejection::UnsupportedDecodeFormat {
                            codec: requirement.codec,
                        }]
                    })
            } else {
                vec![VideoCapabilityRejection::InsufficientStreamMetadata {
                    codec: requirement.codec,
                }]
            }
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
        let raw_outputs = self.raw_video_outputs().collect::<Vec<_>>();
        let playable_outputs = self.supported_video_outputs().collect::<Vec<_>>();

        UnsupportedVideoRequirement {
            requirement: Box::new(requirement.clone()),
            rejections,
            supported_formats_summary: summarize_system_support(
                &raw_outputs,
                &playable_outputs,
                &self.render_backends,
            )
            .into_boxed_str(),
        }
    }

    /// Возвращает transfer/render reject после успешного raw decode match.
    fn output_intersection_rejection(
        &self,
        requirement: &VideoDecodeRequirement,
        frame_format: VideoFramePixelLayout,
        raw_outputs: &[&SupportedVideoOutput],
    ) -> Option<VideoCapabilityRejection> {
        if self.render_backends.is_empty() {
            return Some(VideoCapabilityRejection::NoAvailableRenderer);
        }

        let mut first_render_rejection = None;
        for capabilities in &self.render_backends {
            for output in raw_outputs {
                match capabilities.check_video_output(requirement, output.frame_contract) {
                    Ok(()) => return None,
                    Err(rejection) if first_render_rejection.is_none() => {
                        first_render_rejection = Some((*output, rejection));
                    }
                    Err(_) => {}
                }
            }
        }

        if let Some((output, rejection)) = first_render_rejection {
            if matches!(
                rejection,
                RenderVideoOutputRejection::FrameContract {
                    reason: RenderFrameContractRejection::UnsupportedTransferPath { .. },
                }
            ) && let Some(required_frame_contract) =
                self.renderer_supported_contract_not_backed_by_outputs(requirement, raw_outputs)
            {
                return Some(VideoCapabilityRejection::UnsupportedBackendFrameTransfer {
                    backend: output.backend.clone(),
                    required_frame_contract,
                });
            }

            return Some(render_video_output_rejection_to_capability(
                rejection,
                requirement,
                frame_format,
                output.frame_contract,
                &output.backend,
            ));
        }

        Some(VideoCapabilityRejection::UnsupportedDecodeFormat {
            codec: requirement.codec,
        })
    }

    /// Ищет renderer contract, который подходит stream-у, но не объявлен backend outputs.
    fn renderer_supported_contract_not_backed_by_outputs(
        &self,
        requirement: &VideoDecodeRequirement,
        raw_outputs: &[&SupportedVideoOutput],
    ) -> Option<VideoFrameContract> {
        let renderer_supported_contracts = self
            .render_backends
            .iter()
            .flat_map(|renderer| {
                renderer
                    .supported_frame_contracts
                    .iter()
                    .copied()
                    .filter(move |contract| {
                        renderer.check_video_output(requirement, *contract).is_ok()
                    })
            })
            .collect::<Vec<_>>();

        if renderer_supported_contracts.iter().any(|contract| {
            raw_outputs.iter().any(|output| {
                same_transfer_path_family(
                    output.frame_contract.transfer_path,
                    contract.transfer_path,
                )
            })
        }) {
            return None;
        }

        renderer_supported_contracts.into_iter().find(|contract| {
            !raw_outputs.iter().any(|output| {
                same_transfer_path_family(
                    output.frame_contract.transfer_path,
                    contract.transfer_path,
                )
            })
        })
    }
}

/// Сравнивает transfer family без смешивания layout details.
fn same_transfer_path_family(left: VideoFrameTransferPath, right: VideoFrameTransferPath) -> bool {
    match (left, right) {
        (
            VideoFrameTransferPath::HardwareZeroCopy {
                handle: left_handle,
            },
            VideoFrameTransferPath::HardwareZeroCopy {
                handle: right_handle,
            },
        ) => same_hardware_handle_family(left_handle, right_handle),
        (
            VideoFrameTransferPath::SoftwareHostUpload,
            VideoFrameTransferPath::SoftwareHostUpload,
        ) => true,
        _ => false,
    }
}

/// Сравнивает hardware handle family без layout-specific fields.
fn same_hardware_handle_family(left: HardwareFrameHandle, right: HardwareFrameHandle) -> bool {
    matches!(
        (left, right),
        (
            HardwareFrameHandle::DmaBuf { .. },
            HardwareFrameHandle::DmaBuf { .. }
        )
    )
}

/// Формирует компактный список decode/render возможностей для ошибки.
fn summarize_system_support(
    raw_outputs: &[&SupportedVideoOutput],
    playable_outputs: &[&SupportedVideoOutput],
    render_backends: &[RenderCapabilities],
) -> String {
    let decode_summary = summarize_supported_outputs(raw_outputs);
    let playable_summary = summarize_supported_outputs(playable_outputs);

    format!(
        "raw decode outputs: {decode_summary}; playable outputs: {playable_summary}; render: {}",
        summarize_render_capabilities(render_backends)
    )
}

/// Формирует компактный список поддерживаемых outputs для ошибки.
fn summarize_supported_outputs(outputs: &[&SupportedVideoOutput]) -> String {
    if outputs.is_empty() {
        return "нет доступных video outputs".to_string();
    }

    let mut descriptions = outputs
        .iter()
        .take(6)
        .map(|output| output.describe())
        .collect::<Vec<_>>();

    if outputs.len() > descriptions.len() {
        descriptions.push(format!(
            "ещё {} outputs",
            outputs.len() - descriptions.len()
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
) -> Result<VideoFramePixelLayout, VideoCapabilityRejection> {
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

    video_frame_pixel_layout_from_decode_requirement(requirement).ok_or(
        VideoCapabilityRejection::InsufficientStreamMetadata {
            codec: requirement.codec,
        },
    )
}

/// Проверяет, что requirement действительно требует HDR handling.
fn requirement_requires_hdr_processing(requirement: &VideoDecodeRequirement) -> bool {
    requirement.hdr
        || requirement
            .color
            .as_ref()
            .is_some_and(|color| color.requires_hdr_processing())
}

/// Проверяет strict HDR core metadata до выбора production renderer path.
fn strict_hdr_metadata_rejection(
    requirement: &VideoDecodeRequirement,
    frame_format: VideoFramePixelLayout,
) -> Option<VideoCapabilityRejection> {
    if !requirement_requires_hdr_processing(requirement) {
        return None;
    }

    if frame_format != VideoFramePixelLayout::P010 {
        return Some(invalid_hdr_metadata(
            "Phase 10 HDR-to-SDR принимает только P010 10-bit 4:2:0 input",
        ));
    }

    if requirement.bit_depth != Some(BitDepth::Ten) {
        return Some(invalid_hdr_metadata(
            "strict HDR metadata должна явно указывать 10-bit input",
        ));
    }

    if requirement.chroma != Some(ChromaSubsampling::Yuv420) {
        return Some(invalid_hdr_metadata(
            "strict HDR metadata должна явно указывать YUV 4:2:0 chroma",
        ));
    }

    let Some(color) = requirement.color.as_ref() else {
        return Some(invalid_hdr_metadata(
            "отсутствует resolved color metadata для HDR stream-а",
        ));
    };

    if !matches!(color.transfer, TransferFunction::Pq | TransferFunction::Hlg) {
        return Some(invalid_hdr_metadata(format!(
            "transfer должен быть PQ или HLG, получено {:?}",
            color.transfer
        )));
    }

    if color.primaries != ColorPrimaries::Bt2020 {
        return Some(invalid_hdr_metadata(format!(
            "primaries должны быть BT.2020, получено {:?}",
            color.primaries
        )));
    }

    if color.matrix != MatrixCoefficients::Bt2020 {
        return Some(invalid_hdr_metadata(format!(
            "matrix должна быть BT.2020, получено {:?}",
            color.matrix
        )));
    }

    if !matches!(color.range, ColorRange::Limited | ColorRange::Full) {
        return Some(invalid_hdr_metadata(
            "range должен быть explicit limited или full",
        ));
    }

    if let Some(hdr_metadata) = &color.hdr_metadata {
        if hdr_metadata.color_primaries != color.primaries {
            return Some(invalid_hdr_metadata(format!(
                "HDR side metadata primaries {:?} не совпадают с core primaries {:?}",
                hdr_metadata.color_primaries, color.primaries
            )));
        }

        if hdr_metadata.transfer_function != color.transfer {
            return Some(invalid_hdr_metadata(format!(
                "HDR side metadata transfer {:?} не совпадает с core transfer {:?}",
                hdr_metadata.transfer_function, color.transfer
            )));
        }
    }

    None
}

/// Создаёт typed reject для strict HDR metadata policy.
fn invalid_hdr_metadata(reason: impl Into<String>) -> VideoCapabilityRejection {
    VideoCapabilityRejection::InvalidHdrMetadata {
        reason: reason.into(),
    }
}

/// Переводит neutral render-core rejection в capability-layer user-facing reason.
fn render_video_output_rejection_to_capability(
    rejection: RenderVideoOutputRejection,
    requirement: &VideoDecodeRequirement,
    frame_format: VideoFramePixelLayout,
    frame_contract: VideoFrameContract,
    backend: &DecodeBackendId,
) -> VideoCapabilityRejection {
    match rejection {
        RenderVideoOutputRejection::FrameContract { reason } => {
            render_frame_contract_rejection_to_capability(reason, frame_contract, backend)
        }
        RenderVideoOutputRejection::P010NotRenderable { readiness } => {
            VideoCapabilityRejection::P010NotRenderable { readiness }
        }
        RenderVideoOutputRejection::HdrUnsupported { .. } => {
            VideoCapabilityRejection::UnsupportedHdrRenderer {
                frame_format: Some(frame_format),
            }
        }
        RenderVideoOutputRejection::MaxTextureSizeExceeded {
            max_texture_size, ..
        } => VideoCapabilityRejection::RenderTextureSizeExceeded {
            width: requirement.width,
            height: requirement.height,
            max_texture_size,
        },
    }
}

/// Переводит frame-contract-only отказ renderer-а в capability rejection.
fn render_frame_contract_rejection_to_capability(
    rejection: RenderFrameContractRejection,
    frame_contract: VideoFrameContract,
    backend: &DecodeBackendId,
) -> VideoCapabilityRejection {
    match rejection {
        RenderFrameContractRejection::UnsupportedPixelLayout { pixel_layout } => {
            VideoCapabilityRejection::UnsupportedRenderFrameFormat {
                frame_format: pixel_layout,
            }
        }
        RenderFrameContractRejection::UnsupportedDmaBufImageLayout { image_layout, .. } => {
            VideoCapabilityRejection::UnsupportedDmaBufImageLayout {
                backend: backend.clone(),
                storage_layout: image_layout,
                required_wgpu_feature: dma_buf_layout_required_import_capability_label(
                    image_layout,
                )
                .to_string(),
            }
        }
        RenderFrameContractRejection::InvalidContract { .. }
        | RenderFrameContractRejection::UnsupportedTransferPath { .. }
        | RenderFrameContractRejection::UnsupportedContractCombination { .. } => {
            VideoCapabilityRejection::UnsupportedRenderFrameTransfer { frame_contract }
        }
    }
}

/// Возвращает import capability, без которой renderer не принимает DMA-BUF layout.
fn dma_buf_layout_required_import_capability_label(layout: DmaBufImageLayout) -> &'static str {
    match layout {
        DmaBufImageLayout::SeparateLayers => "TEXTURE_FORMAT_16BIT_NORM",
        DmaBufImageLayout::ComposedLayers => "TEXTURE_FORMAT_P010",
        DmaBufImageLayout::ComposedMultiObject => "MULTI_OBJECT_DMA_BUF_IMPORT",
    }
}

#[cfg(test)]
mod tests;
