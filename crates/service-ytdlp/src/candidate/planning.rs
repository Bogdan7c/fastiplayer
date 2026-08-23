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
    CandidateIdentity, CodecFamily, CodecKind, DynamicRange, StreamLayout, VideoHeight,
    VideoTrackDescriptor, VideoWidth,
};
use web_media_playback_plan::{
    CandidateQualityScore, CandidateRuntimeRequirements, PlanningCandidate,
    PlanningCandidateBuildError, PlanningCandidateSnapshot, PlanningSnapshotBuildError,
};

use super::model::{YtDlpCandidateSnapshot, YtDlpNormalizedCandidate, YtDlpVideoColorEvidence};

/// Ошибка snapshot-level adapter-а до network/player side effects.
///
/// Row-level variants сохранены для source compatibility старого public API.
/// Новые вызовы получают их через [`YtDlpPlanningCandidateRejectionReason`],
/// а [`YtDlpCandidateSnapshot::planning_snapshot`] возвращает только фатальную
/// ошибку сборки итогового neutral snapshot-а.
#[derive(Debug, Error)]
pub enum YtDlpPlanningSnapshotError {
    /// Legacy row-level ошибка: candidate не выразилась через runtime vocabulary.
    #[error("accepted YtDlp candidate не имеет полного runtime requirement")]
    RuntimeRequirement,
    /// Legacy row-level ошибка: planner отверг descriptor/runtime pair.
    #[error("YtDlp candidate нарушает planning contract")]
    Candidate(#[source] PlanningCandidateBuildError),
    /// Итоговый snapshot нарушает source/generation/identity contract.
    #[error("YtDlp planning snapshot нарушает identity contract")]
    Snapshot(#[source] PlanningSnapshotBuildError),
}

/// Причина, по которой одна canonical candidate row не вошла в planner snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum YtDlpPlanningCandidateRejectionReason {
    /// Accepted S19 candidate не удалось выразить через runtime vocabulary.
    #[error("accepted YtDlp candidate не имеет полного runtime requirement")]
    RuntimeRequirement,
    /// Neutral planner отверг несогласованный descriptor/runtime pair.
    #[error("YtDlp candidate нарушает planning contract")]
    Candidate(#[source] PlanningCandidateBuildError),
}

/// Диагностическая запись одной row, локально отклонённой planning adapter-ом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YtDlpPlanningCandidateRejection {
    /// Exact identity связывает отказ с исходной canonical candidate row.
    exact_identity: CandidateIdentity,
    /// Типизированная причина отказа без transport/player side effects.
    reason: YtDlpPlanningCandidateRejectionReason,
}

impl YtDlpPlanningCandidateRejection {
    /// Создаёт row-local отказ только внутри service-owned planning boundary.
    fn new(
        exact_identity: CandidateIdentity,
        reason: YtDlpPlanningCandidateRejectionReason,
    ) -> Self {
        Self {
            exact_identity,
            reason,
        }
    }

    /// Возвращает exact identity отклонённой canonical candidate row.
    #[must_use]
    pub fn exact_identity(&self) -> &CandidateIdentity {
        &self.exact_identity
    }

    /// Возвращает типизированную row-local причину planning rejection.
    #[must_use]
    pub const fn reason(&self) -> YtDlpPlanningCandidateRejectionReason {
        self.reason
    }
}

/// Результат service-owned projection: планируемые rows и локальные отказы соседей.
#[derive(Debug, Clone, PartialEq)]
pub struct YtDlpPlanningProjection {
    /// Neutral snapshot содержит только statically-compatible planning candidates.
    snapshot: PlanningCandidateSnapshot,
    /// Каждый отказ сохраняет identity и точную service-owned причину.
    rejections: Box<[YtDlpPlanningCandidateRejection]>,
}

impl YtDlpPlanningProjection {
    /// Создаёт полную projection после проверки neutral snapshot invariants.
    fn new(
        snapshot: PlanningCandidateSnapshot,
        rejections: Vec<YtDlpPlanningCandidateRejection>,
    ) -> Self {
        Self {
            snapshot,
            rejections: rejections.into_boxed_slice(),
        }
    }

    /// Возвращает neutral planner input без row-local отказов.
    #[must_use]
    pub const fn snapshot(&self) -> &PlanningCandidateSnapshot {
        &self.snapshot
    }

    /// Возвращает все row-local planning rejections в canonical traversal order.
    #[must_use]
    pub fn rejections(&self) -> &[YtDlpPlanningCandidateRejection] {
        &self.rejections
    }

    /// Передаёт neutral snapshot вызывающему коду без дополнительной сборки.
    #[must_use]
    pub fn into_snapshot(self) -> PlanningCandidateSnapshot {
        self.snapshot
    }
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
    /// Строит immutable planner input, не позволяя одной row уничтожить соседей.
    ///
    /// Вызывающему коду, которому нужны typed row-local diagnostics, следует
    /// использовать [`Self::planning_projection`].
    pub fn planning_snapshot(
        &self,
    ) -> Result<PlanningCandidateSnapshot, YtDlpPlanningSnapshotError> {
        self.planning_projection()
            .map(YtDlpPlanningProjection::into_snapshot)
    }

    /// Проецирует accepted rows в planner snapshot и отдельные row-local отказы.
    pub fn planning_projection(
        &self,
    ) -> Result<YtDlpPlanningProjection, YtDlpPlanningSnapshotError> {
        // Успешные rows сохраняют canonical traversal order для neutral planner-а.
        let mut planning_candidates = Vec::new();
        // Ошибочные rows получают собственную диагностику и не влияют на соседей.
        let mut rejections = Vec::new();

        // Каждая accepted row проходит один и тот же production planning adapter.
        for candidate in self.accepted_candidates() {
            // Row-local ошибка не является нарушением целостности всего snapshot-а.
            match planning_candidate(candidate) {
                // Планируемая row остаётся доступной downstream selection.
                Ok(planning_candidate) => planning_candidates.push(planning_candidate),
                // Непланируемая row сохраняет exact identity и точную причину.
                Err(reason) => rejections.push(YtDlpPlanningCandidateRejection::new(
                    candidate.descriptor().identity().clone(),
                    reason,
                )),
            }
        }

        // Source/generation/duplicate identity остаются фатальными snapshot-инвариантами.
        let snapshot =
            PlanningCandidateSnapshot::new(self.source(), self.generation(), planning_candidates)
                .map_err(YtDlpPlanningSnapshotError::Snapshot)?;
        // Возвращаем обе части одной immutable planning projection.
        Ok(YtDlpPlanningProjection::new(snapshot, rejections))
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
) -> Result<PlanningCandidate, YtDlpPlanningCandidateRejectionReason> {
    let runtime = runtime_requirements(
        candidate.descriptor().layout(),
        candidate.video_color_evidence(),
    )?;
    PlanningCandidate::new(
        candidate.descriptor().clone(),
        runtime,
        CandidateQualityScore::new(quality_score(candidate.descriptor().layout())),
    )
    .map_err(YtDlpPlanningCandidateRejectionReason::Candidate)
}

/// Сохраняет exact layout shape и не создаёт несуществующие companion streams.
fn runtime_requirements(
    layout: &StreamLayout,
    video_color_evidence: Option<YtDlpVideoColorEvidence>,
) -> Result<CandidateRuntimeRequirements, YtDlpPlanningCandidateRejectionReason> {
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
) -> Result<VideoDecodeRequirement, YtDlpPlanningCandidateRejectionReason> {
    let CodecKind::Known(codec_family) = track.codec().kind() else {
        return Err(YtDlpPlanningCandidateRejectionReason::RuntimeRequirement);
    };
    let codec = match codec_family {
        CodecFamily::Vp8 => VideoCodec::Vp8,
        CodecFamily::Vp9 => VideoCodec::Vp9,
        CodecFamily::Av1 => VideoCodec::Av1,
        CodecFamily::H264 => VideoCodec::H264,
        CodecFamily::H265 => VideoCodec::H265,
        _ => return Err(YtDlpPlanningCandidateRejectionReason::RuntimeRequirement),
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
) -> Result<VideoDecodeRequirement, YtDlpPlanningCandidateRejectionReason> {
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
                _ => return Err(YtDlpPlanningCandidateRejectionReason::RuntimeRequirement),
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
                _ => return Err(YtDlpPlanningCandidateRejectionReason::RuntimeRequirement),
            };
            Ok(requirement
                .with_profile(VideoProfile::Av1(profile))
                .with_bit_depth(bit_depth(decimal_part(&parts, 3)?)?)
                .with_chroma(ChromaSubsampling::Yuv420))
        }
        // Bare extractor aliases (`AVC1`, `h264`, `V_MPEG4/ISO/AVC`) доказывают
        // только codec family. Profile/depth/chroma обязан дополнить container
        // preflight по codec-private/bitstream evidence до выбора decoder-а.
        VideoCodec::H264 if parts.len() == 1 => Ok(requirement),
        VideoCodec::H264 => h264_profile(requirement, &parts),
        VideoCodec::H265 if raw_codec.starts_with("hev1.1") || raw_codec.starts_with("hvc1.1") => {
            Ok(requirement
                .with_profile(VideoProfile::H265(H265Profile::Main))
                .with_bit_depth(BitDepth::Eight)
                .with_chroma(ChromaSubsampling::Yuv420))
        }
        VideoCodec::H265 => Err(YtDlpPlanningCandidateRejectionReason::RuntimeRequirement),
    }
}

/// Разбирает AVC profile_idc без чтения container/player state.
fn h264_profile(
    requirement: VideoDecodeRequirement,
    parts: &[&str],
) -> Result<VideoDecodeRequirement, YtDlpPlanningCandidateRejectionReason> {
    let avcoti = parts
        .get(1)
        .filter(|value| value.len() == 6)
        .ok_or(YtDlpPlanningCandidateRejectionReason::RuntimeRequirement)?;
    let profile_idc = u8::from_str_radix(&avcoti[0..2], 16)
        .map_err(|_| YtDlpPlanningCandidateRejectionReason::RuntimeRequirement)?;
    let constraint_flags = u8::from_str_radix(&avcoti[2..4], 16)
        .map_err(|_| YtDlpPlanningCandidateRejectionReason::RuntimeRequirement)?;
    let profile =
        h264_profile_from_indication(H264ProfileIndication::new(profile_idc, constraint_flags))
            .map_err(|_| YtDlpPlanningCandidateRejectionReason::RuntimeRequirement)?;
    Ok(requirement
        .with_profile(VideoProfile::H264(profile))
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420))
}

/// Преобразует exact decimal codec parameter без fallback-угадывания.
fn decimal_part(parts: &[&str], index: usize) -> Result<u8, YtDlpPlanningCandidateRejectionReason> {
    parts
        .get(index)
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or(YtDlpPlanningCandidateRejectionReason::RuntimeRequirement)
}

/// Ограничивает bit depth доказанным codec-core набором.
fn bit_depth(bits: u8) -> Result<BitDepth, YtDlpPlanningCandidateRejectionReason> {
    BitDepth::from_bits(bits).ok_or(YtDlpPlanningCandidateRejectionReason::RuntimeRequirement)
}

/// Маппит только exact S20 proven audio families.
fn audio_requirement(
    codec: &web_media_core::NormalizedCodec,
) -> Result<AudioDecodeCodecFamily, YtDlpPlanningCandidateRejectionReason> {
    let CodecKind::Known(family) = codec.kind() else {
        return Err(YtDlpPlanningCandidateRejectionReason::RuntimeRequirement);
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
        _ => Err(YtDlpPlanningCandidateRejectionReason::RuntimeRequirement),
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
