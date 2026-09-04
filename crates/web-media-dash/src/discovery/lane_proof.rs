//! Provider-owned proof одного logical DASH lane до публикации neutral catalog-а.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use dash_mpd_core::{DashMediaKind, DashMpd};
use demux_api::DemuxRegistry;
use media_core::{Demuxer, TrackInfo, TrackKind};
use source_core::HttpRequestTarget;
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveTransportError};
use web_media_core::{
    AudioTrackDescriptor, ChannelCount, CodecFamily, CodecKind, SampleRate, VideoHeight,
    VideoTrackDescriptor, VideoWidth,
};
use web_media_transport_api::SourceGeneration;

use super::DashRepresentationCapabilityProbe;
use crate::catalog::{
    DashLogicalRepresentationSelection, DashRepresentationLaneProbe,
    DashRepresentationLaneProbeError, DashRepresentationLaneProof, DashRepresentationLaneProofPort,
    DashRepresentationLaneTimelineMode, LaneContract, audio_descriptor, dynamic_range,
    normalized_codec, video_descriptor,
};
use crate::component::{DashComponentFactory, DashComponentTrackShapeError};
use crate::plan::{
    DashComponentPlan, DashPresentationPlan, build_manifest_plan_from_logical_selection,
};
use crate::request::DashVodOpenPolicy;

/// Именованный orchestration input скрывает устройство proof owner-а от discovery callers.
pub(super) struct ProviderLaneProofContext<'proof> {
    /// Authoritative parsed presentation, против которой проверяется logical lane.
    pub(super) presentation: &'proof DashMpd,
    /// Effective manifest base после redirect/fetched handoff.
    pub(super) manifest_base: &'proof HttpRequestTarget,
    /// Existing scoped HTTP owner с generation/cancellation semantics.
    pub(super) http: &'proof AdaptiveHttpContext,
    /// Exact source generation текущего discovery attempt-а.
    pub(super) generation: SourceGeneration,
    /// Existing demux registry определяет реально доступные container readers.
    pub(super) demux_registry: &'proof Arc<DemuxRegistry>,
    /// Existing bounded open/probe policy переносится без локальных defaults.
    pub(super) policy: DashVodOpenPolicy,
    /// Existing decoder/audio capability boundary проверяет probed tracks.
    pub(super) capability_probe: &'proof dyn DashRepresentationCapabilityProbe,
    /// Static/dynamic mode сохраняет прежнюю manifest planning semantics.
    pub(super) timeline_mode: DashRepresentationLaneTimelineMode,
}

/// Production implementation neutral lane-proof interface для DASH provider-а.
pub(super) struct ProviderLaneProof<'proof> {
    /// Parsed MPD остаётся immutable и безопасно разделяется bounded workers.
    presentation: &'proof DashMpd,
    /// Effective manifest base применяется существующим plan builder-ом.
    manifest_base: &'proof HttpRequestTarget,
    /// HTTP context остаётся владельцем transport/cancellation/accounting.
    http: &'proof AdaptiveHttpContext,
    /// Generation проверяется существующим transport owner-ом.
    generation: SourceGeneration,
    /// Registry остаётся единственной точкой выбора demux implementation.
    demux_registry: &'proof Arc<DemuxRegistry>,
    /// Policy ограничивает planning, prefix proof и parallelism.
    policy: DashVodOpenPolicy,
    /// Capability probe сохраняет existing composition decision.
    capability_probe: &'proof dyn DashRepresentationCapabilityProbe,
    /// Timeline mode передаётся plan builder-у без переинтерпретации.
    timeline_mode: DashRepresentationLaneTimelineMode,
}

impl<'proof> ProviderLaneProof<'proof> {
    /// Создаёт proof owner из полного именованного orchestration context-а.
    pub(super) fn new(context: ProviderLaneProofContext<'proof>) -> Self {
        let ProviderLaneProofContext {
            presentation,
            manifest_base,
            http,
            generation,
            demux_registry,
            policy,
            capability_probe,
            timeline_mode,
        } = context;
        Self {
            presentation,
            manifest_base,
            http,
            generation,
            demux_registry,
            policy,
            capability_probe,
            timeline_mode,
        }
    }

    /// Доказывает одну lane без mutable состояния: независимые HTTP/demux jobs могут
    /// безопасно разделять immutable MPD, registry и один pooled HTTP source session.
    fn prove_lane_shared(
        &self,
        request: DashRepresentationLaneProbe,
    ) -> Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError> {
        let logical = DashLogicalRepresentationSelection::Single(request.logical_lane);
        let DashPresentationPlan::Single(component_plan) =
            build_manifest_plan_from_logical_selection(
                self.presentation,
                self.manifest_base,
                &logical,
                self.policy.maximum_planned_segments,
                self.timeline_mode,
            )
            .map_err(|_| DashRepresentationLaneProbeError::UnsupportedContainer)?
        else {
            return Err(DashRepresentationLaneProbeError::UnsupportedTrackShape);
        };
        let mut proof = None;
        for period in component_plan.periods.iter().cloned() {
            let period_plan = DashComponentPlan {
                media_kind: component_plan.media_kind,
                periods: vec![period],
                duration: component_plan.duration,
            };
            let factory = DashComponentFactory::new(
                period_plan,
                self.http.clone(),
                self.generation,
                self.policy,
                Arc::clone(self.demux_registry),
            );
            let component = factory
                .open_for_catalog_proof()
                .map_err(|error| map_component_probe_error(&error))?;
            let period_proof =
                prove_tracks(component.tracks(), &request.contract, self.capability_probe)?;
            if proof.as_ref().is_some_and(|proof| proof != &period_proof) {
                return Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict);
            }
            proof = Some(period_proof);
        }
        proof.ok_or(DashRepresentationLaneProbeError::UnsupportedTrackShape)
    }

    /// Выполняет bounded complete pass и восстанавливает manifest-independent lane order.
    fn prove_lanes_bounded(
        &self,
        requests: Vec<DashRepresentationLaneProbe>,
    ) -> Vec<Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError>> {
        let worker_count = self
            .policy
            .maximum_parallel_catalog_probes
            .get()
            .min(requests.len());
        if worker_count <= 1 {
            return requests
                .into_iter()
                .map(|request| self.prove_lane_shared(request))
                .collect();
        }

        let next_request_index = AtomicUsize::new(0);
        let (result_sender, result_receiver) = mpsc::sync_channel(requests.len());
        thread::scope(|scope| {
            let mut workers = Vec::with_capacity(worker_count);
            for worker_index in 0..worker_count {
                let worker_sender = result_sender.clone();
                let worker_requests = &requests;
                let worker_next_index = &next_request_index;
                let worker_name = format!("dash-catalog-probe-{worker_index}");
                let Ok(worker) =
                    thread::Builder::new()
                        .name(worker_name)
                        .spawn_scoped(scope, move || {
                            loop {
                                let request_index =
                                    worker_next_index.fetch_add(1, Ordering::Relaxed);
                                let Some(request) = worker_requests.get(request_index) else {
                                    break;
                                };
                                let outcome = self.prove_lane_shared(request.clone());
                                if worker_sender.send((request_index, outcome)).is_err() {
                                    break;
                                }
                            }
                        })
                else {
                    // Уже запущенные workers подхватят весь remaining batch. Если ОС не
                    // дала создать даже первый поток, ниже missing rows пройдут синхронно.
                    break;
                };
                workers.push(worker);
            }
            drop(result_sender);
            for worker in workers {
                let _ = worker.join();
            }
        });

        let mut indexed_results = result_receiver.into_iter().collect::<Vec<_>>();
        indexed_results.sort_unstable_by_key(|(request_index, _outcome)| *request_index);
        let mut received_results = indexed_results.into_iter().peekable();
        requests
            .into_iter()
            .enumerate()
            .map(|(request_index, request)| {
                if received_results
                    .peek()
                    .is_some_and(|(received_index, _outcome)| *received_index == request_index)
                {
                    return received_results.next().expect("peeked DASH proof result").1;
                }
                // Panic/spawn failure изолируется одной lane и не превращает catalog
                // discovery в hang или silently missing result.
                self.prove_lane_shared(request)
            })
            .collect()
    }
}

impl DashRepresentationLaneProofPort for ProviderLaneProof<'_> {
    fn prove_lane(
        &mut self,
        request: DashRepresentationLaneProbe,
    ) -> Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError> {
        self.prove_lane_shared(request)
    }

    fn prove_lanes(
        &mut self,
        requests: Vec<DashRepresentationLaneProbe>,
    ) -> Vec<Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError>> {
        self.prove_lanes_bounded(requests)
    }
}

/// Сохраняет прежнюю typed классификацию transport/demux probe failures.
fn map_component_probe_error(error: &anyhow::Error) -> DashRepresentationLaneProbeError {
    if let Some(transport) = error.downcast_ref::<AdaptiveTransportError>() {
        return match transport {
            AdaptiveTransportError::Cancelled => DashRepresentationLaneProbeError::Cancelled,
            AdaptiveTransportError::StaleGeneration { .. } => {
                DashRepresentationLaneProbeError::StaleGeneration
            }
            _ => DashRepresentationLaneProbeError::TransportUnavailable,
        };
    }
    if error
        .downcast_ref::<DashComponentTrackShapeError>()
        .is_some()
    {
        return DashRepresentationLaneProbeError::UnsupportedTrackShape;
    }
    DashRepresentationLaneProbeError::UnsupportedContainer
}

/// Доказывает exact track topology, manifest evidence и decoder/audio capability.
fn prove_tracks(
    tracks: &[TrackInfo],
    contract: &LaneContract,
    capability_probe: &dyn DashRepresentationCapabilityProbe,
) -> Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError> {
    let video = exact_track(tracks, TrackKind::Video);
    let audio = exact_track(tracks, TrackKind::Audio);
    match (contract.kind, video, audio, tracks.len()) {
        (DashMediaKind::Video, Some(video), None, 1) => {
            validate_video_track(video, contract)?;
            capability_probe
                .check_video(video)
                .map_err(|_| DashRepresentationLaneProbeError::CapabilityRejected)?;
            Ok(DashRepresentationLaneProof::VideoOnly(
                proven_video_descriptor(video, contract)?,
            ))
        }
        (DashMediaKind::Audio, None, Some(audio), 1) => {
            validate_audio_track(audio, contract)?;
            capability_probe
                .check_audio(audio)
                .map_err(|_| DashRepresentationLaneProbeError::CapabilityRejected)?;
            Ok(DashRepresentationLaneProof::AudioOnly(
                proven_audio_descriptor(audio, contract)?,
            ))
        }
        (DashMediaKind::Muxed, Some(video), Some(audio), 2) => {
            validate_video_track(video, contract)?;
            validate_audio_track(audio, contract)?;
            capability_probe
                .check_muxed(video, audio)
                .map_err(|_| DashRepresentationLaneProbeError::CapabilityRejected)?;
            Ok(DashRepresentationLaneProof::Muxed {
                video: proven_video_descriptor(video, contract)?,
                audio: proven_audio_descriptor(audio, contract)?,
            })
        }
        _ => Err(DashRepresentationLaneProbeError::UnsupportedTrackShape),
    }
}

/// Возвращает единственный track требуемого kind; absent/ambiguous остаются различимы выше.
fn exact_track(tracks: &[TrackInfo], kind: TrackKind) -> Option<&TrackInfo> {
    let mut matches = tracks.iter().filter(|track| track.kind == kind);
    let track = matches.next()?;
    matches.next().is_none().then_some(track)
}

/// Сверяет decoder-relevant video evidence с authoritative manifest contract-ом.
fn validate_video_track(
    track: &TrackInfo,
    contract: &LaneContract,
) -> Result<(), DashRepresentationLaneProbeError> {
    let codec = normalized_codec(
        contract
            .video_codec
            .as_deref()
            .ok_or(DashRepresentationLaneProbeError::ManifestEvidenceConflict)?,
    )
    .map_err(|_| DashRepresentationLaneProbeError::ManifestEvidenceConflict)?;
    if !video_codec_matches(codec.kind(), &track.codec_id) {
        return Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict);
    }
    let probed_width = track.video.as_ref().and_then(|video| video.coded_width);
    let probed_height = track.video.as_ref().and_then(|video| video.coded_height);
    if contract
        .width
        .zip(probed_width)
        .is_some_and(|(advertised, probed)| advertised != probed)
        || contract
            .height
            .zip(probed_height)
            .is_some_and(|(advertised, probed)| advertised != probed)
    {
        return Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict);
    }
    if dynamic_range(contract.color) == web_media_core::DynamicRange::Sdr
        && track
            .video
            .as_ref()
            .and_then(|video| video.color.as_ref())
            .is_some_and(|color| color.requires_hdr_processing())
    {
        return Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict);
    }
    Ok(())
}

/// Сверяет decoder-relevant audio evidence с authoritative manifest contract-ом.
fn validate_audio_track(
    track: &TrackInfo,
    contract: &LaneContract,
) -> Result<(), DashRepresentationLaneProbeError> {
    let codec = normalized_codec(
        contract
            .audio_codec
            .as_deref()
            .ok_or(DashRepresentationLaneProbeError::ManifestEvidenceConflict)?,
    )
    .map_err(|_| DashRepresentationLaneProbeError::ManifestEvidenceConflict)?;
    if !audio_codec_matches(codec.kind(), &track.codec_id)
        || contract
            .audio_sampling_rate
            .zip(track.sample_rate)
            .is_some_and(|(advertised, probed)| advertised != probed)
        || crate::catalog::channel_count(contract.audio_channel_configuration)
            .map(u32::from)
            .zip(track.channels)
            .is_some_and(|(advertised, probed)| advertised != probed)
    {
        return Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict);
    }
    Ok(())
}

/// Строит neutral video descriptor только из согласованных manifest/demux evidence.
fn proven_video_descriptor(
    track: &TrackInfo,
    contract: &LaneContract,
) -> Result<VideoTrackDescriptor, DashRepresentationLaneProbeError> {
    let expected = video_descriptor(contract)
        .map_err(|_| DashRepresentationLaneProbeError::ManifestEvidenceConflict)?;
    let probed = track.video.as_ref();
    let width = probed
        .and_then(|video| video.coded_width)
        .or(expected.width_pixels())
        .map(VideoWidth::new)
        .transpose()
        .map_err(|_| DashRepresentationLaneProbeError::UnsupportedTrackShape)?;
    let height = probed
        .and_then(|video| video.coded_height)
        .map(VideoHeight::new)
        .transpose()
        .map_err(|_| DashRepresentationLaneProbeError::UnsupportedTrackShape)?
        .or(expected.height());
    let dynamic_range = if probed
        .and_then(|video| video.color.as_ref())
        .is_some_and(|color| color.requires_hdr_processing())
    {
        web_media_core::DynamicRange::Hdr
    } else {
        expected.dynamic_range()
    };
    Ok(VideoTrackDescriptor::new(
        expected.codec().clone(),
        width,
        height,
        expected.frame_rate(),
        expected.bitrate(),
        dynamic_range,
    ))
}

/// Строит neutral audio descriptor только из согласованных manifest/demux evidence.
fn proven_audio_descriptor(
    track: &TrackInfo,
    contract: &LaneContract,
) -> Result<AudioTrackDescriptor, DashRepresentationLaneProbeError> {
    let expected = audio_descriptor(contract)
        .map_err(|_| DashRepresentationLaneProbeError::ManifestEvidenceConflict)?;
    let sample_rate = track
        .sample_rate
        .map(SampleRate::new)
        .transpose()
        .map_err(|_| DashRepresentationLaneProbeError::UnsupportedTrackShape)?
        .or(expected.sample_rate());
    let channels = track
        .channels
        .map(|channels| {
            u16::try_from(channels)
                .map_err(|_| DashRepresentationLaneProbeError::UnsupportedTrackShape)
                .and_then(|channels| {
                    ChannelCount::new(channels)
                        .map_err(|_| DashRepresentationLaneProbeError::UnsupportedTrackShape)
                })
        })
        .transpose()?
        .or(expected.channels());
    Ok(AudioTrackDescriptor::new(
        expected.codec().clone(),
        sample_rate,
        channels,
        expected.bitrate(),
        expected.language().cloned(),
    ))
}

/// Сопоставляет normalized manifest video family с demux codec id без расширения profile.
fn video_codec_matches(kind: CodecKind, codec_id: &str) -> bool {
    let normalized = codec_id.trim().to_ascii_uppercase();
    matches!(
        (kind, normalized.as_str()),
        (CodecKind::Known(CodecFamily::Vp8), "V_VP8" | "VP8")
            | (CodecKind::Known(CodecFamily::Vp9), "V_VP9" | "VP9")
            | (CodecKind::Known(CodecFamily::Av1), "V_AV1" | "AV1" | "AV01")
            | (
                CodecKind::Known(CodecFamily::H264),
                "V_MPEG4/ISO/AVC" | "AVC1" | "H264" | "H.264"
            )
            | (
                CodecKind::Known(CodecFamily::H265),
                "V_MPEGH/ISO/HEVC" | "HEV1" | "HVC1" | "H265" | "H.265"
            )
    )
}

/// Сопоставляет normalized manifest audio family с demux codec id без расширения profile.
fn audio_codec_matches(kind: CodecKind, codec_id: &str) -> bool {
    let normalized = codec_id.trim().to_ascii_uppercase();
    matches!(
        (kind, normalized.as_str()),
        (CodecKind::Known(CodecFamily::Opus), "A_OPUS" | "OPUS")
            | (CodecKind::Known(CodecFamily::Vorbis), "A_VORBIS" | "VORBIS")
            | (
                CodecKind::Known(CodecFamily::Aac),
                "A_AAC" | "A_AAC/MPEG2/LC" | "A_AAC/MPEG4/LC" | "AAC"
            )
    )
}

#[cfg(test)]
mod tests {
    use dash_mpd_core::{DashColorMetadata, DashContainer};
    use media_core::TrackId;

    use super::*;
    use crate::discovery::DashRepresentationCapabilityRejection;

    /// Production-shaped capability boundary принимает test audio track без подмены proof logic.
    struct AcceptAllCapabilities;

    impl DashRepresentationCapabilityProbe for AcceptAllCapabilities {
        /// Video в этом focused test не отклоняется capability layer-ом.
        fn check_video(
            &self,
            _video: &TrackInfo,
        ) -> Result<(), DashRepresentationCapabilityRejection> {
            Ok(())
        }

        /// Audio в этом focused test не отклоняется capability layer-ом.
        fn check_audio(
            &self,
            _audio: &TrackInfo,
        ) -> Result<(), DashRepresentationCapabilityRejection> {
            Ok(())
        }

        /// Muxed lane в этом focused test не отклоняется capability layer-ом.
        fn check_muxed(
            &self,
            _video: &TrackInfo,
            _audio: &TrackInfo,
        ) -> Result<(), DashRepresentationCapabilityRejection> {
            Ok(())
        }
    }

    /// Создаёт exact audio-only manifest contract без неявных optional defaults.
    fn audio_contract() -> LaneContract {
        LaneContract {
            kind: DashMediaKind::Audio,
            container: DashContainer::IsoBmff,
            video_codec: None,
            audio_codec: Some("mp4a.40.2".to_owned()),
            bandwidth: Some(128_000),
            width: None,
            height: None,
            frame_rate: None,
            audio_sampling_rate: Some(48_000),
            audio_channel_configuration: None,
            language: None,
            color: DashColorMetadata::default(),
        }
    }

    /// Создаёт demux-visible audio track с явным codec id и sample rate.
    fn audio_track(codec_id: &str) -> TrackInfo {
        TrackInfo {
            id: TrackId::new(1),
            kind: TrackKind::Audio,
            codec_id: codec_id.to_owned(),
            codec_private: None,
            time_base: None,
            duration: None,
            sample_rate: Some(48_000),
            channels: None,
            video: None,
        }
    }

    /// Positive proof и absent/invalid rows сохраняют прежние exact outcome variants.
    #[test]
    fn track_proof_preserves_positive_absent_and_invalid_outcomes() {
        let contract = audio_contract();
        let valid_track = audio_track("A_AAC");
        let valid = prove_tracks(&[valid_track], &contract, &AcceptAllCapabilities);
        assert!(matches!(
            valid,
            Ok(DashRepresentationLaneProof::AudioOnly(_))
        ));

        let absent = prove_tracks(&[], &contract, &AcceptAllCapabilities);
        assert_eq!(
            absent,
            Err(DashRepresentationLaneProbeError::UnsupportedTrackShape)
        );

        let invalid_track = audio_track("A_OPUS");
        let invalid = prove_tracks(&[invalid_track], &contract, &AcceptAllCapabilities);
        assert_eq!(
            invalid,
            Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict)
        );
    }

    /// Component probe failures не сливаются в общий bool или один generic variant.
    #[test]
    fn component_probe_failures_preserve_typed_error_mapping() {
        let cancelled = anyhow::Error::new(AdaptiveTransportError::Cancelled);
        assert_eq!(
            map_component_probe_error(&cancelled),
            DashRepresentationLaneProbeError::Cancelled
        );

        let transport_unavailable = anyhow::Error::new(AdaptiveTransportError::WorkerStopped);
        assert_eq!(
            map_component_probe_error(&transport_unavailable),
            DashRepresentationLaneProbeError::TransportUnavailable
        );

        let invalid_track_shape = anyhow::Error::new(DashComponentTrackShapeError);
        assert_eq!(
            map_component_probe_error(&invalid_track_shape),
            DashRepresentationLaneProbeError::UnsupportedTrackShape
        );

        let unsupported_container = anyhow::Error::msg("test demux probe failure");
        assert_eq!(
            map_component_probe_error(&unsupported_container),
            DashRepresentationLaneProbeError::UnsupportedContainer
        );
    }
}
