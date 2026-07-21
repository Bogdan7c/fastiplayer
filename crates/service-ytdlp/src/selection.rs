//! Product policy выбора YtDlp stream-а до открытия media bytes.

use capability_core::{SelectedVideoStream, SystemCapabilities, UnsupportedVideoRequirement};
use codec_core::{VideoCodec, VideoDecodeRequirement};
use rustiplayer_config::YtDlpHdrSelection;
use thiserror::Error;
use web_media_core::{PreferredHeightPolicy, VideoHeight, VideoHeightError};

use crate::{
    YtDlpDynamicRange, YtDlpStreamCandidate, YtDlpStreamCandidates, YtDlpVideoRequirement,
};

/// Typed причина отказа конкретному service candidate-у.
#[derive(Debug, Clone, PartialEq)]
pub enum YtDlpCandidateRejectionReason {
    /// Adaptive video stream не имеет audio companion-а.
    MissingAudioCompanion,

    /// Service metadata недостаточно для полного codec requirement.
    InsufficientVideoMetadata { reason: String },

    /// Dynamic range или color metadata отсутствует, неоднозначна либо противоречива.
    UnknownDynamicRange,

    /// HDR-кандидат запрещён выбранной пользователем SDR-only политикой.
    HdrDisallowedByPolicy,

    /// Codec отсутствует в пользовательском порядке предпочтения.
    CodecNotPreferred,

    /// Service сообщил высоту за пределами neutral video contract.
    InvalidVideoHeight {
        /// Точная checked-value ошибка neutral owner-а.
        error: VideoHeightError,
    },

    /// Полный decoder/frame-contract/renderer intersection отклонил candidate.
    Capability(UnsupportedVideoRequirement),
}

/// Отказ одному candidate-у с сохранением stable stream identity.
#[derive(Debug, Clone, PartialEq)]
pub struct YtDlpCandidateRejection {
    /// Stable stream id внутри текущего manifest-а.
    pub stream_id: String,

    /// Typed причина, по которой selection продолжила поиск.
    pub reason: YtDlpCandidateRejectionReason,
}

/// Typed итоговая ошибка YtDlp selection policy.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum YtDlpStreamSelectionError {
    /// Service resolver не вернул ни одного candidate-а.
    #[error("YtDlp stream candidates are empty")]
    EmptyCandidates,

    /// В режиме `sdr_only` не осталось playable SDR candidate-ов.
    #[error("YtDlp candidates не содержат playable SDR stream")]
    NoPlayableSdr {
        /// Отказы, объясняющие продолжение поиска по candidate list.
        rejections: Vec<YtDlpCandidateRejection>,
    },

    /// В prefer-HDR режиме не найден ни playable HDR, ни SDR fallback.
    #[error("YtDlp candidates не содержат playable HDR или SDR fallback")]
    NoPlayableCandidate {
        /// Отказы, объясняющие продолжение поиска по candidate list.
        rejections: Vec<YtDlpCandidateRejection>,
    },
}

/// Выбирает stream по HDR/codec/height policy, не открывая media source.
pub fn select_yt_dlp_stream(
    stream_candidates: &YtDlpStreamCandidates,
    preferred_codecs: &[VideoCodec],
    hdr_selection: YtDlpHdrSelection,
    preferred_height: PreferredHeightPolicy,
    system_capabilities: &SystemCapabilities,
) -> Result<SelectedVideoStream, YtDlpStreamSelectionError> {
    if stream_candidates.candidates.is_empty() {
        return Err(YtDlpStreamSelectionError::EmptyCandidates);
    }

    let mut rejections =
        collect_intrinsic_rejections(stream_candidates, preferred_codecs, hdr_selection);
    let dynamic_range_order: &[YtDlpDynamicRange] = match hdr_selection {
        YtDlpHdrSelection::SdrOnly => &[YtDlpDynamicRange::Sdr],
        YtDlpHdrSelection::PreferHdrWhenAvailable => {
            &[YtDlpDynamicRange::Hdr, YtDlpDynamicRange::Sdr]
        }
    };

    for dynamic_range in dynamic_range_order {
        for preferred_codec in preferred_codecs {
            if let Some(selected) = select_best_matching_candidate(
                stream_candidates,
                *preferred_codec,
                *dynamic_range,
                hdr_selection,
                preferred_height,
                system_capabilities,
                &mut rejections,
            ) {
                return Ok(selected);
            }
        }
    }

    Err(match hdr_selection {
        YtDlpHdrSelection::SdrOnly => YtDlpStreamSelectionError::NoPlayableSdr { rejections },
        YtDlpHdrSelection::PreferHdrWhenAvailable => {
            YtDlpStreamSelectionError::NoPlayableCandidate { rejections }
        }
    })
}

/// Собирает отказы, которые не зависят от capability intersection и порядка quality.
fn collect_intrinsic_rejections(
    stream_candidates: &YtDlpStreamCandidates,
    preferred_codecs: &[VideoCodec],
    hdr_selection: YtDlpHdrSelection,
) -> Vec<YtDlpCandidateRejection> {
    stream_candidates
        .candidates
        .iter()
        .filter_map(|candidate| {
            let reason = intrinsic_rejection(candidate, preferred_codecs).or_else(|| {
                let requirement = candidate.video_requirement.as_requirement()?;
                (hdr_selection == YtDlpHdrSelection::SdrOnly
                    && resolved_dynamic_range(candidate, requirement)
                        == Some(YtDlpDynamicRange::Hdr))
                .then_some(YtDlpCandidateRejectionReason::HdrDisallowedByPolicy)
            });

            reason.map(|reason| YtDlpCandidateRejection {
                stream_id: candidate.stream_id.clone(),
                reason,
            })
        })
        .collect()
}

/// Проверяет metadata/audio/codec prerequisites до обращения к capability layer.
fn intrinsic_rejection(
    candidate: &YtDlpStreamCandidate,
    preferred_codecs: &[VideoCodec],
) -> Option<YtDlpCandidateRejectionReason> {
    if candidate.audio.is_none() {
        return Some(YtDlpCandidateRejectionReason::MissingAudioCompanion);
    }

    let YtDlpVideoRequirement::Ready(requirement) = &candidate.video_requirement else {
        let reason = candidate
            .video_requirement
            .insufficient_reason()
            .unwrap_or("service metadata недостаточна")
            .to_string();
        return Some(YtDlpCandidateRejectionReason::InsufficientVideoMetadata { reason });
    };

    if resolved_dynamic_range(candidate, requirement).is_none() {
        return Some(YtDlpCandidateRejectionReason::UnknownDynamicRange);
    }

    if let Err(error) = candidate_video_height(candidate) {
        return Some(YtDlpCandidateRejectionReason::InvalidVideoHeight { error });
    }

    (!preferred_codecs.contains(&requirement.codec))
        .then_some(YtDlpCandidateRejectionReason::CodecNotPreferred)
}

/// Выбирает лучший по height/quality внутри одного codec/dynamic-range bucket-а.
fn select_best_matching_candidate(
    stream_candidates: &YtDlpStreamCandidates,
    preferred_codec: VideoCodec,
    expected_dynamic_range: YtDlpDynamicRange,
    hdr_selection: YtDlpHdrSelection,
    preferred_height: PreferredHeightPolicy,
    system_capabilities: &SystemCapabilities,
    rejections: &mut Vec<YtDlpCandidateRejection>,
) -> Option<SelectedVideoStream> {
    let mut ordered_candidates = stream_candidates.candidates.iter().collect::<Vec<_>>();
    ordered_candidates.sort_by(|left, right| {
        preferred_height
            .compare(
                candidate_video_height(left).ok().flatten(),
                candidate_video_height(right).ok().flatten(),
            )
            .then_with(|| right.quality_score.cmp(&left.quality_score))
            .then_with(|| left.stream_id.cmp(&right.stream_id))
    });

    for candidate in ordered_candidates {
        if intrinsic_rejection(candidate, &[preferred_codec]).is_some() {
            continue;
        }

        let YtDlpVideoRequirement::Ready(requirement) = &candidate.video_requirement else {
            continue;
        };
        let Some(dynamic_range) = resolved_dynamic_range(candidate, requirement) else {
            continue;
        };
        if dynamic_range != expected_dynamic_range || requirement.codec != preferred_codec {
            continue;
        }
        if hdr_selection == YtDlpHdrSelection::SdrOnly && dynamic_range == YtDlpDynamicRange::Hdr {
            rejections.push(YtDlpCandidateRejection {
                stream_id: candidate.stream_id.clone(),
                reason: YtDlpCandidateRejectionReason::HdrDisallowedByPolicy,
            });
            continue;
        }

        match system_capabilities.check_video_requirement(requirement) {
            Ok(matched_output) => {
                return Some(SelectedVideoStream {
                    stream_id: candidate.stream_id.clone(),
                    requirement: requirement.clone(),
                    matched_output: matched_output.clone(),
                });
            }
            Err(rejection) => rejections.push(YtDlpCandidateRejection {
                stream_id: candidate.stream_id.clone(),
                reason: YtDlpCandidateRejectionReason::Capability(rejection),
            }),
        }
    }

    None
}

/// Валидирует optional service height через neutral web-media owner.
fn candidate_video_height(
    candidate: &YtDlpStreamCandidate,
) -> Result<Option<VideoHeight>, VideoHeightError> {
    candidate.height.map(VideoHeight::new).transpose()
}

/// Принимает dynamic range только когда отдельный manifest field согласован с typed color metadata.
fn resolved_dynamic_range(
    candidate: &YtDlpStreamCandidate,
    requirement: &VideoDecodeRequirement,
) -> Option<YtDlpDynamicRange> {
    let color_metadata = requirement.color.as_ref()?;
    let color_dynamic_range = if color_metadata.requires_hdr_processing() {
        YtDlpDynamicRange::Hdr
    } else {
        YtDlpDynamicRange::Sdr
    };

    (candidate.dynamic_range == color_dynamic_range).then_some(color_dynamic_range)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use capability_core::{
        BackendCapabilities, BackendDriverInfo, BackendProbeStatus,
        CURRENT_CAPABILITY_SCHEMA_VERSION, SupportedVideoOutput,
    };
    use codec_core::{
        BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, DecodeBackendId,
        MatrixCoefficients, SupportedVideoDecodeFormat, TransferFunction, VideoColorMetadata,
        VideoProfile, Vp9Profile,
    };
    use source_core::SourceValidators;
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    use super::*;
    use crate::{YtDlpDirectStreamDescriptor, YtDlpStreamKind};

    /// Строит capability snapshot, где decoder знает SDR/HDR, а renderer HDR support управляем.
    fn capabilities(hdr_renderer_compatible: bool) -> SystemCapabilities {
        let sdr_output = supported_output(
            Vp9Profile::Profile0,
            BitDepth::Eight,
            false,
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        );
        let hdr_output = supported_output(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            true,
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
        );
        let raw_supported_outputs = vec![sdr_output.clone(), hdr_output.clone()];
        let mut playable_video_outputs = vec![sdr_output];
        if hdr_renderer_compatible {
            playable_video_outputs.push(hdr_output);
        }

        SystemCapabilities {
            schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
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
            render_backends: Vec::new(),
            playable_video_outputs,
        }
    }

    /// Создаёт один decoder output с точным frame contract-ом.
    fn supported_output(
        profile: Vp9Profile,
        bit_depth: BitDepth,
        hdr_input: bool,
        frame_contract: VideoFrameContract,
    ) -> SupportedVideoOutput {
        SupportedVideoOutput {
            backend: DecodeBackendId::vaapi(),
            decode_format: SupportedVideoDecodeFormat {
                codec: VideoCodec::Vp9,
                profile: VideoProfile::Vp9(profile),
                bit_depth,
                chroma: ChromaSubsampling::Yuv420,
                max_width: Some(4096),
                max_height: Some(2304),
                max_fps: None,
                hdr_input,
            },
            frame_contract,
        }
    }

    /// Полный VP9 requirement с достоверной typed color metadata.
    fn vp9_requirement(is_hdr: bool) -> VideoDecodeRequirement {
        let (profile, bit_depth, color) = if is_hdr {
            (
                Vp9Profile::Profile2,
                BitDepth::Ten,
                VideoColorMetadata::container(
                    ColorRange::Limited,
                    MatrixCoefficients::Bt2020,
                    ColorPrimaries::Bt2020,
                    TransferFunction::Pq,
                    None,
                ),
            )
        } else {
            (
                Vp9Profile::Profile0,
                BitDepth::Eight,
                VideoColorMetadata::sdr_bt709_limited(),
            )
        };

        VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(profile))
            .with_bit_depth(bit_depth)
            .with_chroma(ChromaSubsampling::Yuv420)
            .with_resolution(1920, 1080)
            .with_frame_rate(60.0)
            .with_color(color)
    }

    /// Строит открываемый candidate с отдельными video/audio descriptors.
    fn candidate(stream_id: &str, is_hdr: bool, quality_score: i64) -> YtDlpStreamCandidate {
        let dynamic_range = if is_hdr {
            YtDlpDynamicRange::Hdr
        } else {
            YtDlpDynamicRange::Sdr
        };
        YtDlpStreamCandidate {
            stream_id: stream_id.to_string(),
            format_id: Some(format!("{stream_id}+audio")),
            video: descriptor(YtDlpStreamKind::Video, stream_id),
            audio: Some(descriptor(YtDlpStreamKind::Audio, "audio")),
            height: Some(1080),
            fps: Some(60.0),
            vcodec: Some("vp09".to_string()),
            acodec: Some("opus".to_string()),
            dynamic_range,
            video_requirement: YtDlpVideoRequirement::Ready(vp9_requirement(is_hdr)),
            quality_score,
        }
    }

    /// Строит candidate с согласованной manifest/capability высотой.
    fn candidate_with_height(
        stream_id: &str,
        is_hdr: bool,
        height: u32,
        quality_score: i64,
    ) -> YtDlpStreamCandidate {
        let mut candidate = candidate(stream_id, is_hdr, quality_score);
        candidate.height = Some(height);
        candidate.video_requirement = YtDlpVideoRequirement::Ready(
            vp9_requirement(is_hdr).with_resolution(height.saturating_mul(16) / 9, height),
        );
        candidate
    }

    /// Строит inert descriptor: selection обязана читать metadata, но не URL bytes.
    fn descriptor(kind: YtDlpStreamKind, format_id: &str) -> YtDlpDirectStreamDescriptor {
        YtDlpDirectStreamDescriptor {
            kind,
            url: crate::YtDlpDirectStreamUrl::from_secret_for_open(format!(
                "https://media.invalid/{format_id}"
            )),
            headers: Vec::new(),
            format_id: Some(format_id.to_string()),
            service_media_id: Some("media".to_string()),
            validators: SourceValidators::default(),
            duration: Some(Duration::from_secs(60)),
            live: false,
            description: format_id.to_string(),
        }
    }

    /// Оборачивает candidates в manifest-level service DTO.
    fn stream_candidates(candidates: Vec<YtDlpStreamCandidate>) -> YtDlpStreamCandidates {
        YtDlpStreamCandidates {
            title: Some("test".to_string()),
            service_media_id: Some("media".to_string()),
            duration: Some(Duration::from_secs(60)),
            live: false,
            candidates,
        }
    }

    #[test]
    fn prefer_hdr_selects_hdr_only_when_full_capability_intersection_passes() {
        let candidates = stream_candidates(vec![
            candidate("sdr", false, 10_000),
            candidate("hdr", true, 100),
        ]);

        let selected = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::PreferHdrWhenAvailable,
            PreferredHeightPolicy::NoPreference,
            &capabilities(true),
        )
        .expect("playable HDR is preferred before SDR regardless of quality score");

        assert_eq!(selected.stream_id, "hdr");
    }

    #[test]
    fn prefer_hdr_falls_back_to_sdr_when_renderer_cannot_play_hdr() {
        let candidates = stream_candidates(vec![
            candidate("hdr", true, 10_000),
            candidate("sdr", false, 100),
        ]);

        let selected = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::PreferHdrWhenAvailable,
            PreferredHeightPolicy::NoPreference,
            &capabilities(false),
        )
        .expect("renderer-incompatible HDR must fall back to playable SDR");

        assert_eq!(selected.stream_id, "sdr");
    }

    #[test]
    fn sdr_only_never_selects_hdr_and_reports_hdr_only_list() {
        let candidates = stream_candidates(vec![candidate("hdr", true, 10_000)]);

        let error = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::SdrOnly,
            PreferredHeightPolicy::NoPreference,
            &capabilities(true),
        )
        .expect_err("SdrOnly must reject even playable HDR");

        assert!(matches!(
            error,
            YtDlpStreamSelectionError::NoPlayableSdr { rejections }
                if rejections.iter().any(|rejection| {
                    rejection.stream_id == "hdr"
                        && rejection.reason == YtDlpCandidateRejectionReason::HdrDisallowedByPolicy
                })
        ));
    }

    #[test]
    fn prefer_hdr_hdr_only_requires_playable_hdr_intersection() {
        let candidates = stream_candidates(vec![candidate("hdr", true, 10_000)]);

        let selected = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::PreferHdrWhenAvailable,
            PreferredHeightPolicy::NoPreference,
            &capabilities(true),
        )
        .expect("HDR-only list is playable when full intersection passes");
        assert_eq!(selected.stream_id, "hdr");

        let error = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::PreferHdrWhenAvailable,
            PreferredHeightPolicy::NoPreference,
            &capabilities(false),
        )
        .expect_err("HDR-only list without renderer intersection has no SDR fallback");
        assert!(matches!(
            error,
            YtDlpStreamSelectionError::NoPlayableCandidate { rejections }
                if rejections.iter().any(|rejection| matches!(
                    rejection.reason,
                    YtDlpCandidateRejectionReason::Capability(_)
                ))
        ));
    }

    #[test]
    fn unknown_dynamic_range_is_rejected_and_selection_continues() {
        let mut unknown = candidate("unknown", false, 10_000);
        unknown.dynamic_range = YtDlpDynamicRange::Unknown;
        let candidates = stream_candidates(vec![unknown, candidate("sdr", false, 100)]);

        let selected = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::SdrOnly,
            PreferredHeightPolicy::NoPreference,
            &capabilities(false),
        )
        .expect("UnknownDynamicRange must not stop search before later SDR");

        assert_eq!(selected.stream_id, "sdr");
    }

    #[test]
    fn missing_color_metadata_returns_typed_unknown_dynamic_range_failure() {
        let mut unknown = candidate("unknown", false, 10_000);
        unknown.video_requirement = YtDlpVideoRequirement::Ready(
            VideoDecodeRequirement::new(VideoCodec::Vp9)
                .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
                .with_bit_depth(BitDepth::Eight)
                .with_chroma(ChromaSubsampling::Yuv420),
        );
        let candidates = stream_candidates(vec![unknown]);

        let error = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::SdrOnly,
            PreferredHeightPolicy::NoPreference,
            &capabilities(false),
        )
        .expect_err("missing typed color metadata must not be guessed as SDR");

        assert!(matches!(
            error,
            YtDlpStreamSelectionError::NoPlayableSdr { rejections }
                if rejections.iter().any(|rejection| {
                    rejection.stream_id == "unknown"
                        && rejection.reason == YtDlpCandidateRejectionReason::UnknownDynamicRange
                })
        ));
    }

    #[test]
    fn preferred_height_uses_exact_then_lower_then_higher_before_quality_score() {
        let policy = PreferredHeightPolicy::Prefer(
            web_media_core::PreferredVideoHeight::new(1080).expect("1080 валидно"),
        );
        let exact_candidates = stream_candidates(vec![
            candidate_with_height("higher", false, 1440, 10_000),
            candidate_with_height("lower", false, 720, 20_000),
            candidate_with_height("exact", false, 1080, 1),
        ]);
        let exact = select_yt_dlp_stream(
            &exact_candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::SdrOnly,
            policy,
            &capabilities(false),
        )
        .expect("exact height должна победить quality score");
        assert_eq!(exact.stream_id, "exact");

        let fallback_candidates = stream_candidates(vec![
            candidate_with_height("higher-near", false, 1200, 30_000),
            candidate_with_height("lower-far", false, 480, 40_000),
            candidate_with_height("lower-near", false, 720, 1),
        ]);
        let lower = select_yt_dlp_stream(
            &fallback_candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::SdrOnly,
            policy,
            &capabilities(false),
        )
        .expect("closest lower должна победить любой higher fallback");
        assert_eq!(lower.stream_id, "lower-near");

        let higher_candidates = stream_candidates(vec![
            candidate_with_height("higher-far", false, 2160, 50_000),
            candidate_with_height("higher-near", false, 1200, 1),
        ]);
        let higher = select_yt_dlp_stream(
            &higher_candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::SdrOnly,
            policy,
            &capabilities(false),
        )
        .expect("closest higher должна завершить fallback");
        assert_eq!(higher.stream_id, "higher-near");
    }

    #[test]
    fn no_height_preference_keeps_best_playable_quality_order() {
        let candidates = stream_candidates(vec![
            candidate_with_height("lower-quality", false, 1080, 1),
            candidate_with_height("best-quality", false, 720, 10_000),
        ]);

        let selected = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::SdrOnly,
            PreferredHeightPolicy::NoPreference,
            &capabilities(false),
        )
        .expect("None preference должна сохранить BestPlayable ordering");

        assert_eq!(selected.stream_id, "best-quality");
    }

    #[test]
    fn hdr_bucket_remains_stronger_than_preferred_height() {
        let candidates = stream_candidates(vec![
            candidate_with_height("sdr-exact", false, 1080, 100_000),
            candidate_with_height("hdr-lower", true, 720, 1),
        ]);
        let policy = PreferredHeightPolicy::Prefer(
            web_media_core::PreferredVideoHeight::new(1080).expect("1080 валидно"),
        );

        let selected = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::PreferHdrWhenAvailable,
            policy,
            &capabilities(true),
        )
        .expect("playable HDR bucket должен остаться первым");

        assert_eq!(selected.stream_id, "hdr-lower");
    }

    #[test]
    fn codec_policy_filters_exact_height_before_height_ranking() {
        let mut omitted_codec = candidate_with_height("h264-exact", false, 1080, 100_000);
        omitted_codec.video_requirement = YtDlpVideoRequirement::Ready(
            VideoDecodeRequirement::new(VideoCodec::H264)
                .with_resolution(1920, 1080)
                .with_color(VideoColorMetadata::sdr_bt709_limited()),
        );
        let candidates = stream_candidates(vec![
            omitted_codec,
            candidate_with_height("vp9-lower", false, 720, 1),
        ]);
        let policy = PreferredHeightPolicy::Prefer(
            web_media_core::PreferredVideoHeight::new(1080).expect("1080 валидно"),
        );

        let selected = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::SdrOnly,
            policy,
            &capabilities(false),
        )
        .expect("height не должна вернуть codec вне configured policy");

        assert_eq!(selected.stream_id, "vp9-lower");
    }

    #[test]
    fn invalid_candidate_height_is_typed_rejection() {
        let candidates = stream_candidates(vec![candidate_with_height("zero", false, 0, 1)]);

        let error = select_yt_dlp_stream(
            &candidates,
            &[VideoCodec::Vp9],
            YtDlpHdrSelection::SdrOnly,
            PreferredHeightPolicy::NoPreference,
            &capabilities(false),
        )
        .expect_err("zero height не должна превращаться в missing metadata");

        assert!(matches!(
            error,
            YtDlpStreamSelectionError::NoPlayableSdr { rejections }
                if rejections.iter().any(|rejection| matches!(
                    rejection.reason,
                    YtDlpCandidateRejectionReason::InvalidVideoHeight {
                        error: VideoHeightError::Zero
                    }
                ))
        ));
    }
}
