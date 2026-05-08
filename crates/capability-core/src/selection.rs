use codec_core::{SupportedVideoDecodeFormat, VideoCodec, VideoDecodeRequirement, VideoProfile};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::SystemCapabilities;

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

    /// Codec отсутствует во всей matrix.
    UnsupportedCodec {
        /// Запрошенный codec.
        codec: VideoCodec,
    },

    /// Codec есть, но нужного profile нет.
    UnsupportedProfile {
        /// Запрошенный profile.
        profile: VideoProfile,
    },

    /// Codec/profile есть, но bit depth/chroma/HDR/resolution не совпали.
    UnsupportedFormat {
        /// Описание несовпадения.
        details: String,
    },
}

impl VideoCapabilityRejection {
    /// Формирует короткий текст причины для UI.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::NoAvailableBackend => "hardware video backend недоступен".to_string(),
            Self::UnsupportedCodec { codec } => format!("codec {codec} не поддерживается"),
            Self::UnsupportedProfile { profile } => {
                format!("profile {profile} не поддерживается аппаратным backend-ом")
            }
            Self::UnsupportedFormat { details } => details.clone(),
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

    /// Проверяет одно stream requirement и возвращает detailed error при отказе.
    pub fn check_video_requirement(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> Result<&SupportedVideoDecodeFormat, UnsupportedVideoRequirement> {
        if let Some(format) = self.find_supported_video_format(requirement) {
            return Ok(format);
        }

        Err(self.explain_unsupported_video_requirement(requirement))
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

        for candidate in ordered_candidates {
            if let Some(format) = self.find_supported_video_format(&candidate.requirement) {
                return Ok(SelectedVideoStream {
                    stream_id: candidate.stream_id.clone(),
                    requirement: candidate.requirement.clone(),
                    matched_format: format.clone(),
                });
            }
        }

        Err(StreamSelectionError::Unsupported(
            self.explain_unsupported_video_requirement(&candidates[0].requirement),
        ))
    }

    /// Строит structured rejection list без потери исходного requirement.
    #[must_use]
    pub fn explain_unsupported_video_requirement(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> UnsupportedVideoRequirement {
        let supported_formats = self.supported_video_formats().collect::<Vec<_>>();
        let rejections = if supported_formats.is_empty() {
            vec![VideoCapabilityRejection::NoAvailableBackend]
        } else if !supported_formats
            .iter()
            .any(|format| format.codec == requirement.codec)
        {
            vec![VideoCapabilityRejection::UnsupportedCodec {
                codec: requirement.codec,
            }]
        } else if let Some(profile) = requirement.profile {
            if !supported_formats
                .iter()
                .any(|format| format.codec == requirement.codec && format.profile == profile)
            {
                vec![VideoCapabilityRejection::UnsupportedProfile { profile }]
            } else {
                vec![VideoCapabilityRejection::UnsupportedFormat {
                    details: format!(
                        "codec/profile найден, но bit depth/chroma/resolution/HDR не совпали"
                    ),
                }]
            }
        } else {
            vec![VideoCapabilityRejection::UnsupportedFormat {
                details: "codec найден, но stream metadata недостаточно точна для выбора"
                    .to_string(),
            }]
        };

        UnsupportedVideoRequirement {
            requirement: requirement.clone(),
            rejections,
            supported_formats_summary: summarize_supported_formats(&supported_formats),
        }
    }
}

/// Формирует компактный список поддерживаемых форматов для ошибки.
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

#[cfg(test)]
mod tests {
    use codec_core::{
        BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoCodec,
        VideoDecodeRequirement, VideoProfile, Vp9Profile,
    };

    use crate::{BackendCapabilities, BackendDriverInfo, BackendProbeStatus, SystemCapabilities};

    use super::*;

    fn capabilities_with_vp9_profile0() -> SystemCapabilities {
        SystemCapabilities {
            schema_version: crate::CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![BackendCapabilities {
                backend_id: DecodeBackendId::vaapi(),
                display_name: "VA-API".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                supported_video_decode_formats: vec![SupportedVideoDecodeFormat {
                    codec: VideoCodec::Vp9,
                    profile: VideoProfile::Vp9(Vp9Profile::Profile0),
                    bit_depth: BitDepth::Eight,
                    chroma: ChromaSubsampling::Yuv420,
                    max_width: Some(1920),
                    max_height: Some(1080),
                    max_fps: None,
                    hdr_input: false,
                    backend: DecodeBackendId::vaapi(),
                }],
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                export_paths: Vec::new(),
                diagnostics: Vec::new(),
            }],
        }
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
}
