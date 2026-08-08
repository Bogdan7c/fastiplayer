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
mod tests {
    use codec_core::{
        Av1Profile, BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, DecodeBackendId,
        HdrMetadata, MatrixCoefficients, SupportedVideoDecodeFormat, TransferFunction, VideoCodec,
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
        )
    }

    fn capabilities_with_formats(
        supported_formats: Vec<SupportedVideoDecodeFormat>,
        render_backends: Vec<RenderCapabilities>,
    ) -> SystemCapabilities {
        let raw_supported_outputs = supported_formats
            .into_iter()
            .filter_map(output_for_supported_format)
            .collect::<Vec<_>>();
        capabilities_with_outputs(raw_supported_outputs, render_backends)
    }

    fn capabilities_with_outputs(
        raw_supported_outputs: Vec<SupportedVideoOutput>,
        render_backends: Vec<RenderCapabilities>,
    ) -> SystemCapabilities {
        let playable_video_outputs = raw_supported_outputs
            .iter()
            .filter(|output| {
                let mut requirement = VideoDecodeRequirement::new(output.decode_format.codec)
                    .with_profile(output.decode_format.profile)
                    .with_bit_depth(output.decode_format.bit_depth)
                    .with_chroma(output.decode_format.chroma);
                requirement.hdr = output.decode_format.hdr_input;
                render_backends.iter().any(|renderer| {
                    renderer.supports_video_output(&requirement, output.frame_contract)
                })
            })
            .cloned()
            .collect::<Vec<_>>();

        SystemCapabilities {
            schema_version: crate::CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![BackendCapabilities {
                backend_id: DecodeBackendId::vaapi(),
                display_name: "VA-API".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                raw_supported_outputs,
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                diagnostics: Vec::new(),
            }],
            render_backends,
            playable_video_outputs,
        }
    }

    fn output_for_supported_format(
        decode_format: SupportedVideoDecodeFormat,
    ) -> Option<SupportedVideoOutput> {
        let frame_contract = match (decode_format.bit_depth, decode_format.chroma) {
            (BitDepth::Eight, ChromaSubsampling::Yuv420) => {
                VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers)
            }
            (BitDepth::Ten, ChromaSubsampling::Yuv420) => {
                VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers)
            }
            _ => return None,
        };

        Some(SupportedVideoOutput {
            backend: DecodeBackendId::vaapi(),
            decode_format,
            frame_contract,
        })
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
        }
    }

    fn av1_format(
        profile: Av1Profile,
        bit_depth: BitDepth,
        chroma: ChromaSubsampling,
        hdr_input: bool,
    ) -> SupportedVideoDecodeFormat {
        SupportedVideoDecodeFormat {
            codec: VideoCodec::Av1,
            profile: VideoProfile::Av1(profile),
            bit_depth,
            chroma,
            max_width: Some(4096),
            max_height: Some(2304),
            max_fps: None,
            hdr_input,
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

    fn bt709_limited_with_content_light_metadata() -> VideoColorMetadata {
        VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt709,
            ColorPrimaries::Bt709,
            TransferFunction::Bt709,
            Some(HdrMetadata {
                color_primaries: ColorPrimaries::Bt709,
                transfer_function: TransferFunction::Bt709,
                max_luminance_nits: None,
                min_luminance_nits: None,
                max_content_light_level_nits: Some(1_100),
                max_frame_average_light_level_nits: Some(180),
            }),
        )
    }

    #[test]
    fn exact_backend_lookup_returns_matching_playable_output() {
        let capabilities = capabilities_with_vp9_profile0();
        let requirement = vp9_requirement(
            Vp9Profile::Profile0,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
        );
        let vaapi_backend_id = DecodeBackendId::vaapi();

        let selected_output = capabilities
            .find_playable_video_output_for_backend(&vaapi_backend_id, &requirement)
            .expect("exact playable VA-API output должен быть найден");

        assert_eq!(selected_output.backend, vaapi_backend_id);
        assert!(selected_output.satisfies(&requirement));
    }

    #[test]
    fn exact_backend_lookup_rejects_output_owned_by_another_backend() {
        let capabilities = capabilities_with_vp9_profile0();
        let requirement = vp9_requirement(
            Vp9Profile::Profile0,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
        );
        let software_backend_id = DecodeBackendId::new("ffmpeg")
            .expect("canonical software backend id должен быть валиден");

        assert!(
            capabilities
                .find_playable_video_output_for_backend(&software_backend_id, &requirement)
                .is_none()
        );
    }

    #[test]
    fn exact_backend_lookup_rejects_requirement_mismatch() {
        let capabilities = capabilities_with_vp9_profile0();
        let unsupported_requirement = vp9_requirement(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
        );
        let vaapi_backend_id = DecodeBackendId::vaapi();

        assert!(
            capabilities
                .find_playable_video_output_for_backend(
                    &vaapi_backend_id,
                    &unsupported_requirement,
                )
                .is_none()
        );
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
    fn vp9_profile0_bt709_with_content_light_metadata_stays_on_sdr_nv12_path() {
        let capabilities = capabilities_with_vp9_profile0();
        let requirement = vp9_requirement(
            Vp9Profile::Profile0,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
        )
        .with_resolution(3840, 2160)
        .with_color(bt709_limited_with_content_light_metadata());
        let candidates = vec![VideoStreamCandidate {
            stream_id: "sdr-vp9-profile0".to_string(),
            requirement: requirement.clone(),
            quality_score: 10,
        }];

        let selected = capabilities
            .select_best_video_stream(&candidates)
            .expect("BT.709 SDR with content-light side metadata must stay playable");

        assert!(!requirement.hdr);
        assert_eq!(selected.stream_id, "sdr-vp9-profile0");
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
        let message = error.user_message();
        assert!(message.contains("profile VP9 Profile 2"));
        assert!(message.contains("доступными video decode backend-ами"));
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
        let mut p010_without_hdr_renderer =
            RenderCapabilities::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
                Some(4096),
                vec![DmaBufImageLayout::SeparateLayers],
            );
        p010_without_hdr_renderer.supports_hdr_to_sdr = false;
        p010_without_hdr_renderer
            .supported_hdr_to_sdr_operators
            .clear();

        let capabilities = capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            )],
            vec![p010_without_hdr_renderer],
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
                frame_format: Some(VideoFramePixelLayout::P010),
            })
        ));
        assert!(error.user_message().contains("HDR-to-SDR renderer"));
    }

    #[test]
    fn yuv420_requirement_accepts_backend_software_host_upload_contract() {
        let capabilities = capabilities_with_outputs(
            vec![SupportedVideoOutput {
                backend: DecodeBackendId::vaapi(),
                decode_format: vp9_format(
                    Vp9Profile::Profile0,
                    BitDepth::Eight,
                    ChromaSubsampling::Yuv420,
                    false,
                ),
                frame_contract: VideoFrameContract::host_yuv420_planar8(),
            }],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
        );
        let requirement = vp9_requirement(
            Vp9Profile::Profile0,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
        );

        let selected_output = capabilities
            .check_video_requirement(&requirement)
            .expect("YUV420 software host-upload output is renderable");

        assert_eq!(
            selected_output.frame_contract,
            VideoFrameContract::host_yuv420_planar8()
        );
    }

    #[test]
    fn p010_boundary_verified_state_alone_does_not_make_stream_playable() {
        let mut render_capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
        render_capabilities.p010_render_readiness = P010RenderReadiness::ZeroCopyBoundaryVerified;
        render_capabilities
            .supported_frame_contracts
            .push(VideoFrameContract::dma_buf_p010(
                DmaBufImageLayout::SeparateLayers,
            ));
        let capabilities = capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                false,
            )],
            vec![render_capabilities],
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
    fn p010_renderable_bt2446c_renderer_makes_hdr_to_sdr_stream_playable() {
        let capabilities = capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            )],
            vec![RenderCapabilities::wgpu_p010_bt2446c(Some(4096))],
        );
        let requirement = vp9_requirement(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
        )
        .with_color(bt2020_pq_limited());

        let selected_output = capabilities
            .check_video_requirement(&requirement)
            .expect("P010 renderable + BT.2446-C must enable HDR-to-SDR playback");

        assert_eq!(selected_output.decode_format.bit_depth, BitDepth::Ten);
        assert_eq!(
            selected_output.decode_format.chroma,
            ChromaSubsampling::Yuv420
        );
        assert!(selected_output.decode_format.hdr_input);
    }

    #[test]
    fn hdr_stream_is_selected_only_when_decode_p010_layout_and_hdr_to_sdr_pass() {
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
            vec![
                RenderCapabilities::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
                    Some(4096),
                    vec![DmaBufImageLayout::SeparateLayers],
                ),
            ],
        );
        let candidates = vec![
            VideoStreamCandidate {
                stream_id: "sdr".to_string(),
                requirement: vp9_requirement(
                    Vp9Profile::Profile0,
                    BitDepth::Eight,
                    ChromaSubsampling::Yuv420,
                ),
                quality_score: 10,
            },
            VideoStreamCandidate {
                stream_id: "hdr".to_string(),
                requirement: vp9_requirement(
                    Vp9Profile::Profile2,
                    BitDepth::Ten,
                    ChromaSubsampling::Yuv420,
                )
                .with_color(bt2020_pq_limited()),
                quality_score: 100,
            },
        ];

        let selected = capabilities
            .select_best_video_stream(&candidates)
            .expect("HDR stream should be selected when full Phase 10 intersection passes");

        assert_eq!(selected.stream_id, "hdr");
    }

    #[test]
    fn hdr_stream_is_skipped_when_p010_layout_feature_is_missing() {
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
            vec![
                RenderCapabilities::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
                    Some(4096),
                    vec![DmaBufImageLayout::ComposedLayers],
                ),
            ],
        );
        let candidates = vec![
            VideoStreamCandidate {
                stream_id: "sdr".to_string(),
                requirement: vp9_requirement(
                    Vp9Profile::Profile0,
                    BitDepth::Eight,
                    ChromaSubsampling::Yuv420,
                ),
                quality_score: 10,
            },
            VideoStreamCandidate {
                stream_id: "hdr".to_string(),
                requirement: vp9_requirement(
                    Vp9Profile::Profile2,
                    BitDepth::Ten,
                    ChromaSubsampling::Yuv420,
                )
                .with_color(bt2020_pq_limited()),
                quality_score: 100,
            },
        ];

        let selected = capabilities
            .select_best_video_stream(&candidates)
            .expect("SDR fallback candidate should be selected instead of unsupported HDR layout");

        assert_eq!(selected.stream_id, "sdr");
    }

    #[test]
    fn missing_separate_layer_p010_import_feature_rejects_hdr_stream() {
        let capabilities = capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            )],
            vec![
                RenderCapabilities::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
                    Some(4096),
                    vec![DmaBufImageLayout::ComposedLayers],
                ),
            ],
        );
        let requirement = vp9_requirement(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
        )
        .with_color(bt2020_pq_limited());

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("baseline separate-layer P010 must require TEXTURE_FORMAT_16BIT_NORM");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedDmaBufImageLayout {
                storage_layout: DmaBufImageLayout::SeparateLayers,
                required_wgpu_feature,
                ..
            }) if required_wgpu_feature == "TEXTURE_FORMAT_16BIT_NORM"
        ));
    }

    #[test]
    fn missing_composed_p010_import_feature_rejects_hdr_stream() {
        let capabilities = capabilities_with_outputs(
            vec![SupportedVideoOutput {
                backend: DecodeBackendId::vaapi(),
                decode_format: vp9_format(
                    Vp9Profile::Profile2,
                    BitDepth::Ten,
                    ChromaSubsampling::Yuv420,
                    true,
                ),
                frame_contract: VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
            }],
            vec![
                RenderCapabilities::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
                    Some(4096),
                    vec![DmaBufImageLayout::SeparateLayers],
                ),
            ],
        );
        let requirement = vp9_requirement(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
        )
        .with_color(bt2020_pq_limited());

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("compatibility composed P010 must require TEXTURE_FORMAT_P010");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedDmaBufImageLayout {
                storage_layout: DmaBufImageLayout::ComposedLayers,
                required_wgpu_feature,
                ..
            }) if required_wgpu_feature == "TEXTURE_FORMAT_P010"
        ));
    }

    #[test]
    fn known_multi_object_contract_is_rejected_before_decode_start() {
        let capabilities = capabilities_with_outputs(
            vec![SupportedVideoOutput {
                backend: DecodeBackendId::vaapi(),
                decode_format: vp9_format(
                    Vp9Profile::Profile0,
                    BitDepth::Eight,
                    ChromaSubsampling::Yuv420,
                    false,
                ),
                frame_contract: VideoFrameContract::dma_buf_nv12(
                    DmaBufImageLayout::ComposedMultiObject,
                ),
            }],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
        );
        let requirement = vp9_requirement(
            Vp9Profile::Profile0,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
        );

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("known multi-object output must not enter the playable capability set");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedDmaBufImageLayout {
                storage_layout: DmaBufImageLayout::ComposedMultiObject,
                required_wgpu_feature,
                ..
            }) if required_wgpu_feature == "MULTI_OBJECT_DMA_BUF_IMPORT"
        ));
    }

    #[test]
    fn missing_strict_hdr_metadata_rejects_stream_before_render() {
        let capabilities = capabilities_with_formats(
            vec![vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            )],
            vec![RenderCapabilities::wgpu_p010_bt2446c(Some(4096))],
        );
        let mut requirement = vp9_requirement(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
        );
        requirement.hdr = true;

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("HDR stream without resolved strict metadata must be rejected");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::InvalidHdrMetadata { reason })
                if reason.contains("отсутствует resolved color metadata")
        ));
    }

    #[test]
    fn unsupported_av1_profile_is_reported_before_decode_start() {
        let capabilities = capabilities_with_formats(
            vec![av1_format(
                Av1Profile::Main,
                BitDepth::Eight,
                ChromaSubsampling::Yuv420,
                false,
            )],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
        );
        let requirement = VideoDecodeRequirement::new(VideoCodec::Av1)
            .with_profile(VideoProfile::Av1(Av1Profile::High))
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420);

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("AV1 High must be rejected by AV1 Main-only capabilities");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedProfile {
                codec: VideoCodec::Av1,
                profile: VideoProfile::Av1(Av1Profile::High),
            })
        ));
    }

    #[test]
    fn codec_neutral_p010_surface_without_renderer_support_is_rejected_before_decode_start() {
        let capabilities = capabilities_with_formats(
            vec![av1_format(
                Av1Profile::Main,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            )],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
        );
        let requirement = VideoDecodeRequirement::new(VideoCodec::Av1)
            .with_profile(VideoProfile::Av1(Av1Profile::Main))
            .with_bit_depth(BitDepth::Ten)
            .with_chroma(ChromaSubsampling::Yuv420);

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err(
                "P010 surface must be rejected before hardware decode if renderer cannot import it",
            );

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedRenderFrameFormat {
                frame_format: VideoFramePixelLayout::P010,
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
