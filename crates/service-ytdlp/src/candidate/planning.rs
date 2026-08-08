//! Provider-specific mapping S19 snapshot-а в neutral S21C planning vocabulary.

use std::collections::{BTreeMap, BTreeSet};

use audio_core::AudioDecodeCodecFamily;
use codec_core::{
    Av1Profile, BitDepth, ChromaSubsampling, ColorMetadataConfidence, ColorMetadataOrigin,
    ColorPrimaries, ColorRange, H264ProfileIndication, H265Profile, MatrixCoefficients,
    TransferFunction, VideoCodec, VideoColorMetadata, VideoDecodeRequirement, VideoProfile,
    Vp8Profile, Vp9Profile, h264_profile_from_indication,
};
use thiserror::Error;
use web_media_core::{
    CodecFamily, CodecKind, DynamicRange, StreamLayout, VideoHeight, VideoTrackDescriptor,
    VideoWidth,
};
use web_media_playback_plan::{
    CandidateQualityScore, CandidateRuntimeRequirements, PlanningCandidate,
    PlanningCandidateBuildError, PlanningCandidateSnapshot, PlanningSnapshotBuildError,
};

use super::model::{YtDlpCandidateSnapshot, YtDlpNormalizedCandidate, YtDlpVideoColorEvidence};

/// Ошибка adapter-а до network/player side effects.
#[derive(Debug, Error)]
pub enum YtDlpPlanningSnapshotError {
    /// Accepted S19 candidate не удалось выразить через runtime vocabulary.
    #[error("accepted YtDlp candidate не имеет полного runtime requirement")]
    RuntimeRequirement,
    /// Neutral planner отверг несогласованный descriptor/runtime pair.
    #[error("YtDlp candidate нарушает planning contract")]
    Candidate(#[source] PlanningCandidateBuildError),
    /// Итоговый snapshot нарушает source/generation/identity contract.
    #[error("YtDlp planning snapshot нарушает identity contract")]
    Snapshot(#[source] PlanningSnapshotBuildError),
}

/// Ошибка correspondence между service snapshot и переданным planner snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum YtDlpPlanningSnapshotAlignmentError {
    /// Planner snapshot принадлежит другой source lineage.
    #[error("planner snapshot принадлежит другой source lineage")]
    SourceMismatch,
    /// Planner snapshot принадлежит другой extraction generation.
    #[error("planner snapshot принадлежит другой extraction generation")]
    GenerationMismatch,
    /// Полный exact+semantic candidate set не совпадает с canonical service view.
    #[error("planner snapshot не соответствует canonical service candidate set")]
    CandidateIdentityMismatch,
    /// Planner row сохранил identity, но изменил service-owned projection.
    #[error("planner snapshot изменил canonical service candidate projection")]
    CandidateProjectionMismatch,
    /// Canonical service candidate неожиданно перестал проецироваться в planner vocabulary.
    #[error("canonical service candidate не удалось спроецировать в planner snapshot")]
    ServiceProjectionFailed,
}

impl YtDlpCandidateSnapshot {
    /// Строит immutable planner input из canonical accepted snapshot view.
    pub fn planning_snapshot(
        &self,
    ) -> Result<PlanningCandidateSnapshot, YtDlpPlanningSnapshotError> {
        let planning_candidates = self
            .accepted_candidates()
            .map(planning_candidate)
            .collect::<Result<Vec<_>, _>>()?;
        PlanningCandidateSnapshot::new(self.source(), self.generation(), planning_candidates)
            .map_err(YtDlpPlanningSnapshotError::Snapshot)
    }

    /// Проверяет полный order-independent service-owned projection до app-side use.
    pub fn validate_planning_snapshot_alignment(
        &self,
        planning_snapshot: &PlanningCandidateSnapshot,
    ) -> Result<(), YtDlpPlanningSnapshotAlignmentError> {
        if planning_snapshot.source() != self.source() {
            return Err(YtDlpPlanningSnapshotAlignmentError::SourceMismatch);
        }
        if planning_snapshot.generation() != self.generation() {
            return Err(YtDlpPlanningSnapshotAlignmentError::GenerationMismatch);
        }

        let service_projection = self
            .planning_snapshot()
            .map_err(|_| YtDlpPlanningSnapshotAlignmentError::ServiceProjectionFailed)?;
        let service_candidates = service_projection
            .candidates()
            .iter()
            .map(|candidate| (candidate.descriptor().identity().clone(), candidate))
            .collect::<BTreeMap<_, _>>();
        let planning_candidates = planning_snapshot
            .candidates()
            .iter()
            .map(|candidate| (candidate.descriptor().identity().clone(), candidate))
            .collect::<BTreeMap<_, _>>();
        let service_identities = service_candidates
            .iter()
            .map(|(exact, candidate)| {
                (
                    exact.clone(),
                    candidate.descriptor().semantic_identity().clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        let planning_identities = planning_candidates
            .iter()
            .map(|(exact, candidate)| {
                (
                    exact.clone(),
                    candidate.descriptor().semantic_identity().clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        if service_identities != planning_identities {
            return Err(YtDlpPlanningSnapshotAlignmentError::CandidateIdentityMismatch);
        }
        if service_candidates != planning_candidates {
            return Err(YtDlpPlanningSnapshotAlignmentError::CandidateProjectionMismatch);
        }
        Ok(())
    }

    /// Находит canonical service candidate для exact planner identity pair.
    pub fn canonical_candidate_for_planning_identity(
        &self,
        exact_identity: &web_media_core::CandidateIdentity,
        semantic_identity: &web_media_core::SemanticIdentity,
    ) -> Option<&YtDlpNormalizedCandidate> {
        self.accepted_candidates().find(|candidate| {
            candidate.descriptor().identity() == exact_identity
                && candidate.descriptor().semantic_identity() == semantic_identity
        })
    }
}

/// Связывает descriptor, decode requirements и deterministic service quality.
fn planning_candidate(
    candidate: &YtDlpNormalizedCandidate,
) -> Result<PlanningCandidate, YtDlpPlanningSnapshotError> {
    let runtime = runtime_requirements(
        candidate.descriptor().layout(),
        candidate.video_color_evidence(),
    )?;
    PlanningCandidate::new(
        candidate.descriptor().clone(),
        runtime,
        CandidateQualityScore::new(quality_score(candidate.descriptor().layout())),
    )
    .map_err(YtDlpPlanningSnapshotError::Candidate)
}

/// Сохраняет exact layout shape и не создаёт несуществующие companion streams.
fn runtime_requirements(
    layout: &StreamLayout,
    video_color_evidence: Option<YtDlpVideoColorEvidence>,
) -> Result<CandidateRuntimeRequirements, YtDlpPlanningSnapshotError> {
    match layout {
        StreamLayout::Muxed(component) => Ok(CandidateRuntimeRequirements::Muxed {
            video: video_requirement(component.video(), video_color_evidence)?,
            audio: audio_requirement(component.audio().codec())?,
        }),
        StreamLayout::HlsMuxedCodecDeferred(_) => {
            Ok(CandidateRuntimeRequirements::HlsMuxedCodecDeferred)
        }
        StreamLayout::ContentProbed(component) => Ok(CandidateRuntimeRequirements::ContentProbed {
            video: component
                .video()
                .declared()
                .map(|video| video_requirement(video, video_color_evidence))
                .transpose()?,
            audio: component
                .audio()
                .declared()
                .map(|audio| audio_requirement(audio.codec()))
                .transpose()?,
        }),
        StreamLayout::Separate { video, audio } => Ok(CandidateRuntimeRequirements::Separate {
            video: video_requirement(video.video(), video_color_evidence)?,
            audio: audio_requirement(audio.audio().codec())?,
        }),
        StreamLayout::VideoOnly(component) => Ok(CandidateRuntimeRequirements::VideoOnly {
            video: video_requirement(component.video(), video_color_evidence)?,
        }),
        StreamLayout::AudioOnly(component) => Ok(CandidateRuntimeRequirements::AudioOnly {
            audio: audio_requirement(component.audio().codec())?,
        }),
    }
}

/// Строит conservative capability query из S19 normalized codec tag-а.
fn video_requirement(
    track: &VideoTrackDescriptor,
    video_color_evidence: Option<YtDlpVideoColorEvidence>,
) -> Result<VideoDecodeRequirement, YtDlpPlanningSnapshotError> {
    let CodecKind::Known(codec_family) = track.codec().kind() else {
        return Err(YtDlpPlanningSnapshotError::RuntimeRequirement);
    };
    let codec = match codec_family {
        CodecFamily::Vp8 => VideoCodec::Vp8,
        CodecFamily::Vp9 => VideoCodec::Vp9,
        CodecFamily::Av1 => VideoCodec::Av1,
        CodecFamily::H264 => VideoCodec::H264,
        CodecFamily::H265 => VideoCodec::H265,
        _ => return Err(YtDlpPlanningSnapshotError::RuntimeRequirement),
    };
    let mut requirement = VideoDecodeRequirement::new(codec);
    if let (Some(width), Some(height)) = (track.width_pixels(), track.height()) {
        requirement = requirement.with_resolution(width, height.pixels());
    }
    if let Some(frame_rate) = track.frame_rate() {
        let fps = f64::from(frame_rate.numerator()) / f64::from(frame_rate.denominator());
        requirement = requirement.with_frame_rate(fps);
    }
    requirement = apply_codec_profile(requirement, track)?;
    match track.dynamic_range() {
        DynamicRange::Sdr => {
            requirement = requirement.with_color(VideoColorMetadata::sdr_bt709_limited());
        }
        DynamicRange::Hdr => match video_color_evidence {
            Some(evidence) => requirement = requirement.with_color(hdr_color(evidence)),
            None => requirement.hdr = true,
        },
        DynamicRange::Unknown => {}
    }
    Ok(requirement)
}

/// Переводит exact extractor evidence в strict capability metadata без provider I/O.
fn hdr_color(evidence: YtDlpVideoColorEvidence) -> VideoColorMetadata {
    let transfer = match evidence {
        YtDlpVideoColorEvidence::Bt2020PqLimited => TransferFunction::Pq,
        YtDlpVideoColorEvidence::Bt2020HlgLimited => TransferFunction::Hlg,
    };
    VideoColorMetadata {
        range: ColorRange::Limited,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer,
        hdr_metadata: None,
        origin: ColorMetadataOrigin::Manifest,
        confidence: ColorMetadataConfidence::Hint,
    }
}

/// Извлекает только доказанные profile/bit-depth/chroma поля из codec identity.
fn apply_codec_profile(
    requirement: VideoDecodeRequirement,
    track: &VideoTrackDescriptor,
) -> Result<VideoDecodeRequirement, YtDlpPlanningSnapshotError> {
    let raw_codec = track.codec().raw().as_str();
    let parts = raw_codec.split('.').collect::<Vec<_>>();
    match requirement.codec {
        VideoCodec::Vp8 => Ok(requirement
            .with_profile(VideoProfile::Vp8(Vp8Profile::Version0To3))
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420)),
        VideoCodec::Vp9 if raw_codec == "vp9" => Ok(requirement
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420)),
        // yt-dlp публикует YouTube HDR Profile 2 в сокращённой форме без RFC codec fields.
        // Явный HDR доказывает 10-bit для этого extractor shape; без него fail-closed сохраняется.
        VideoCodec::Vp9 if raw_codec == "vp9.2" && track.dynamic_range() == DynamicRange::Hdr => {
            Ok(requirement
                .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
                .with_bit_depth(BitDepth::Ten)
                .with_chroma(ChromaSubsampling::Yuv420))
        }
        VideoCodec::Vp9 => {
            let profile = match decimal_part(&parts, 1)? {
                0 => Vp9Profile::Profile0,
                1 => Vp9Profile::Profile1,
                2 => Vp9Profile::Profile2,
                3 => Vp9Profile::Profile3,
                _ => return Err(YtDlpPlanningSnapshotError::RuntimeRequirement),
            };
            Ok(requirement
                .with_profile(VideoProfile::Vp9(profile))
                .with_bit_depth(bit_depth(decimal_part(&parts, 3)?)?)
                .with_chroma(ChromaSubsampling::Yuv420))
        }
        VideoCodec::Av1 => {
            let profile = match decimal_part(&parts, 1)? {
                0 => Av1Profile::Main,
                1 => Av1Profile::High,
                2 => Av1Profile::Professional,
                _ => return Err(YtDlpPlanningSnapshotError::RuntimeRequirement),
            };
            Ok(requirement
                .with_profile(VideoProfile::Av1(profile))
                .with_bit_depth(bit_depth(decimal_part(&parts, 3)?)?)
                .with_chroma(ChromaSubsampling::Yuv420))
        }
        VideoCodec::H264 => h264_profile(requirement, &parts),
        VideoCodec::H265 if raw_codec.starts_with("hev1.1") || raw_codec.starts_with("hvc1.1") => {
            Ok(requirement
                .with_profile(VideoProfile::H265(H265Profile::Main))
                .with_bit_depth(BitDepth::Eight)
                .with_chroma(ChromaSubsampling::Yuv420))
        }
        VideoCodec::H265 => Err(YtDlpPlanningSnapshotError::RuntimeRequirement),
    }
}

/// Разбирает AVC profile_idc без чтения container/player state.
fn h264_profile(
    requirement: VideoDecodeRequirement,
    parts: &[&str],
) -> Result<VideoDecodeRequirement, YtDlpPlanningSnapshotError> {
    let avcoti = parts
        .get(1)
        .filter(|value| value.len() == 6)
        .ok_or(YtDlpPlanningSnapshotError::RuntimeRequirement)?;
    let profile_idc = u8::from_str_radix(&avcoti[0..2], 16)
        .map_err(|_| YtDlpPlanningSnapshotError::RuntimeRequirement)?;
    let constraint_flags = u8::from_str_radix(&avcoti[2..4], 16)
        .map_err(|_| YtDlpPlanningSnapshotError::RuntimeRequirement)?;
    let profile =
        h264_profile_from_indication(H264ProfileIndication::new(profile_idc, constraint_flags))
            .map_err(|_| YtDlpPlanningSnapshotError::RuntimeRequirement)?;
    Ok(requirement
        .with_profile(VideoProfile::H264(profile))
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420))
}

/// Преобразует exact decimal codec parameter без fallback-угадывания.
fn decimal_part(parts: &[&str], index: usize) -> Result<u8, YtDlpPlanningSnapshotError> {
    parts
        .get(index)
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or(YtDlpPlanningSnapshotError::RuntimeRequirement)
}

/// Ограничивает bit depth доказанным codec-core набором.
fn bit_depth(bits: u8) -> Result<BitDepth, YtDlpPlanningSnapshotError> {
    BitDepth::from_bits(bits).ok_or(YtDlpPlanningSnapshotError::RuntimeRequirement)
}

/// Маппит только exact S20 proven audio families.
fn audio_requirement(
    codec: &web_media_core::NormalizedCodec,
) -> Result<AudioDecodeCodecFamily, YtDlpPlanningSnapshotError> {
    let CodecKind::Known(family) = codec.kind() else {
        return Err(YtDlpPlanningSnapshotError::RuntimeRequirement);
    };
    match family {
        CodecFamily::Aac => Ok(AudioDecodeCodecFamily::Aac),
        CodecFamily::Adpcm => Ok(AudioDecodeCodecFamily::Adpcm),
        CodecFamily::Alac => Ok(AudioDecodeCodecFamily::Alac),
        CodecFamily::Flac => Ok(AudioDecodeCodecFamily::Flac),
        CodecFamily::Mp1 => Ok(AudioDecodeCodecFamily::Mp1),
        CodecFamily::Mp2 => Ok(AudioDecodeCodecFamily::Mp2),
        CodecFamily::Mp3 => Ok(AudioDecodeCodecFamily::Mp3),
        CodecFamily::Opus => Ok(AudioDecodeCodecFamily::Opus),
        CodecFamily::Pcm => Ok(AudioDecodeCodecFamily::Pcm),
        CodecFamily::Vorbis => Ok(AudioDecodeCodecFamily::Vorbis),
        _ => Err(YtDlpPlanningSnapshotError::RuntimeRequirement),
    }
}

fn quality_score(layout: &StreamLayout) -> i64 {
    match layout {
        StreamLayout::HlsMuxedCodecDeferred(component) => {
            let height = i64::from(component.height().pixels());
            let width = i64::from(component.width().map_or(0, VideoWidth::pixels));
            let fps = component.frame_rate().map_or(0, |frame_rate| {
                (f64::from(frame_rate.numerator()) * 1_000.0 / f64::from(frame_rate.denominator()))
                    as i64
            });
            let bitrate = component
                .bitrate()
                .map_or(0, |bitrate| (bitrate.bits_per_second() / 1_000) as i64);
            height
                .saturating_mul(1_000_000)
                .saturating_add(width.saturating_mul(1_000))
                .saturating_add(fps)
                .saturating_add(bitrate)
        }
        StreamLayout::ContentProbed(component) => {
            let hints = component.video_hints();
            let height = i64::from(hints.height().map_or(0, VideoHeight::pixels));
            let width = i64::from(hints.width().map_or(0, VideoWidth::pixels));
            let fps = hints.frame_rate().map_or(0, |frame_rate| {
                (f64::from(frame_rate.numerator()) * 1_000.0 / f64::from(frame_rate.denominator()))
                    as i64
            });
            let bitrate = hints
                .bitrate()
                .and_then(|bitrate| i64::try_from(bitrate.bits_per_second()).ok())
                .unwrap_or(0);
            height * 1_000_000_000 + width * 1_000_000 + fps * 1_000 + bitrate
        }
        _ => quality_score_from_video_track(layout),
    }
}

/// Сохраняет прежний service ordering для layout-ов с video track descriptor.
fn quality_score_from_video_track(layout: &StreamLayout) -> i64 {
    let video = match layout {
        StreamLayout::Muxed(component) => Some(component.video()),
        StreamLayout::Separate { video, .. } | StreamLayout::VideoOnly(video) => {
            Some(video.video())
        }
        StreamLayout::ContentProbed(component) => component.video().declared(),
        StreamLayout::AudioOnly(_) | StreamLayout::HlsMuxedCodecDeferred(_) => None,
    };
    let Some(video) = video else {
        return 0;
    };
    let height = i64::from(
        video
            .height()
            .map_or(0, web_media_core::VideoHeight::pixels),
    );
    let width = i64::from(video.width_pixels().unwrap_or(0));
    let fps = video.frame_rate().map_or(0, |frame_rate| {
        (f64::from(frame_rate.numerator()) * 1_000.0 / f64::from(frame_rate.denominator())) as i64
    });
    let bitrate = video
        .bitrate()
        .map_or(0, |bitrate| (bitrate.bits_per_second() / 1_000) as i64);
    height
        .saturating_mul(1_000_000)
        .saturating_add(width.saturating_mul(1_000))
        .saturating_add(fps)
        .saturating_add(bitrate)
}
