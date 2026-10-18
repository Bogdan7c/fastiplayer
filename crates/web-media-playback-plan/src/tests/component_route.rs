//! Separate video/audio пара playable только при общем runtime composition route-е.
//!
//! Регрессия реального YouTube `616+251`: yt-dlp выбрал HLS video-only (`m3u8_native`)
//! вместе с progressive HTTPS audio-only. Каждый component по отдельности поддержан,
//! поэтому planner раньше ставил пару первой (полный A/V выигрывает у VideoOnly),
//! а runtime падал terminal-ошибкой «non-progressive transport family».

use audio_core::{AudioDecodeCapabilitySnapshot, AudioDecodeCodecFamily};
use codec_core::VideoCodec;
use demux_api::{DemuxInputCapabilities, DemuxInputCapability};
use web_media_core::{
    AudioComponentDescriptor, ContainerFamily, DynamicRange, HttpScheme, PreferredHeightPolicy,
    SelectionRequest, StreamLayout, StreamLayoutKind, TransportFamily, VideoComponentDescriptor,
};

use super::support::{
    VideoCandidateSpec, audio_track, candidate_descriptor, candidate_snapshot, container,
    exact_request, sdr_requirement, selection_policy, separate_candidate, supported_video_format,
    transport, video_capabilities, video_only_candidate, video_track,
};
use crate::{
    CandidateCapabilityRejection, CandidateQualityScore, CandidateRejectionReason,
    CandidateRuntimeRequirements, DemuxCapabilityRegistration, DemuxCapabilitySnapshot,
    HdrSelectionPolicy, PlanningCandidate, PlaybackCapabilitySnapshot, PlaybackPlanningError,
    TransportCapabilityRegistration, TransportCapabilitySnapshot, plan_playback,
};

/// Transport/demux snapshot, где и HLS/fMP4, и progressive HTTPS/WebM поддержаны.
fn hls_and_progressive_capabilities() -> (TransportCapabilitySnapshot, DemuxCapabilitySnapshot) {
    let progressive_outputs = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
        .with(DemuxInputCapability::StreamingBytes);
    let ordered_outputs = DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments);
    let transport = TransportCapabilitySnapshot::new(vec![
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveHttp(HttpScheme::Https),
            progressive_outputs,
        )
        .expect("HTTPS transport registration valid"),
        TransportCapabilityRegistration::new(TransportFamily::Hls, ordered_outputs)
            .expect("HLS transport registration valid"),
    ]);
    let demux = DemuxCapabilitySnapshot::new(vec![
        DemuxCapabilityRegistration::new(ContainerFamily::WebM, progressive_outputs)
            .expect("WebM demux registration valid"),
        DemuxCapabilityRegistration::new(ContainerFamily::IsoBmff, ordered_outputs)
            .expect("ISO BMFF demux registration valid"),
    ]);
    (transport, demux)
}

/// Separate кандидат той же формы, что YouTube `616+251`: HLS video + HTTPS audio.
fn mixed_hls_video_https_audio_candidate() -> PlanningCandidate {
    let video = VideoComponentDescriptor::new(
        transport("m3u8_native"),
        container("mp4"),
        video_track("vp09.00.40.08", 1080, DynamicRange::Sdr),
    );
    let audio = AudioComponentDescriptor::new(transport("https"), container("webm"), audio_track());
    PlanningCandidate::new(
        candidate_descriptor(
            "616+251",
            "mixed-hls-https",
            StreamLayout::Separate { video, audio },
        ),
        CandidateRuntimeRequirements::Separate {
            video: sdr_requirement(VideoCodec::Vp9, 1080),
            audio: AudioDecodeCodecFamily::Opus,
        },
        // Самый высокий quality score: без проверки маршрута пара выиграла бы ranking.
        CandidateQualityScore::new(1_000),
    )
    .expect("mixed separate candidate passes static admission")
}

/// Progressive VP9 video-only строка, на которую должен уйти BestPlayable.
fn progressive_video_only_candidate() -> PlanningCandidate {
    video_only_candidate(VideoCandidateSpec {
        format_id: "248",
        semantic_key: "progressive-vp9-1080",
        transport_raw: "https",
        container_raw: "webm",
        codec_raw: "vp09.00.40.08",
        height: 1080,
        dynamic_range: DynamicRange::Sdr,
        requirement: sdr_requirement(VideoCodec::Vp9, 1080),
        quality_score: 10,
    })
}

/// Проверяет, что среди причин есть ровно одна typed route-mismatch причина нужной пары.
fn assert_single_route_mismatch(reasons: &[CandidateRejectionReason]) {
    let route_mismatches = reasons
        .iter()
        .filter(|reason| {
            matches!(
                reason,
                CandidateRejectionReason::Capability(
                    CandidateCapabilityRejection::ComponentRouteMismatch {
                        video: TransportFamily::Hls,
                        audio: TransportFamily::ProgressiveHttp(HttpScheme::Https),
                    }
                )
            )
        })
        .count();
    assert_eq!(route_mismatches, 1, "unexpected reasons: {reasons:?}");
    assert_eq!(
        reasons.len(),
        1,
        "per-component transport/demux/decode layers must stay accepted: {reasons:?}"
    );
}

/// BestPlayable не выбирает смешанную HLS+HTTPS пару и сообщает typed причину.
#[test]
fn best_playable_rejects_mixed_hls_https_separate_pair() {
    let (transport_capabilities, demux_capabilities) = hls_and_progressive_capabilities();
    let video = video_capabilities(vec![supported_video_format(VideoCodec::Vp9, false)]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport_capabilities,
        &demux_capabilities,
        &video,
        AudioDecodeCapabilitySnapshot::empty().with_available_family(AudioDecodeCodecFamily::Opus),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::WebM],
    );
    let snapshot = candidate_snapshot(vec![
        mixed_hls_video_https_audio_candidate(),
        progressive_video_only_candidate(),
    ]);

    let outcome = plan_playback(
        &snapshot,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("progressive video-only alternative stays playable");

    assert_eq!(outcome.selected().layout(), StreamLayoutKind::VideoOnly);
    assert_eq!(outcome.rejected_candidates().len(), 1);
    assert_single_route_mismatch(outcome.rejected_candidates()[0].reasons());
}

/// Exact intent на смешанную пару падает planner-ошибкой, а не terminal runtime open-ом.
#[test]
fn exact_mixed_hls_https_separate_pair_is_not_playable() {
    let (transport_capabilities, demux_capabilities) = hls_and_progressive_capabilities();
    let video = video_capabilities(vec![supported_video_format(VideoCodec::Vp9, false)]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport_capabilities,
        &demux_capabilities,
        &video,
        AudioDecodeCapabilitySnapshot::empty().with_available_family(AudioDecodeCodecFamily::Opus),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::WebM],
    );
    let mixed = mixed_hls_video_https_audio_candidate();
    let request = exact_request(&mixed);
    let snapshot = candidate_snapshot(vec![mixed]);

    let PlaybackPlanningError::ExactCandidateNotPlayable(rejection) =
        plan_playback(&snapshot, capabilities, &request, &policy)
            .expect_err("mixed separate pair has no runtime route")
    else {
        panic!("exact candidate rejection expected");
    };

    assert_single_route_mismatch(rejection.reasons());
}

/// Однородная progressive пара по-прежнему выигрывает у VideoOnly (A/V completeness).
#[test]
fn homogeneous_progressive_separate_pair_stays_best_playable() {
    let (transport_capabilities, demux_capabilities) = hls_and_progressive_capabilities();
    let video = video_capabilities(vec![supported_video_format(VideoCodec::Vp9, false)]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport_capabilities,
        &demux_capabilities,
        &video,
        AudioDecodeCapabilitySnapshot::empty().with_available_family(AudioDecodeCodecFamily::Opus),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::WebM],
    );
    let snapshot = candidate_snapshot(vec![
        progressive_video_only_candidate(),
        separate_candidate("248+251", "progressive-separate"),
    ]);

    let outcome = plan_playback(
        &snapshot,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("homogeneous separate pair is playable");

    assert_eq!(outcome.selected().layout(), StreamLayoutKind::Separate);
    assert!(outcome.rejected_candidates().is_empty());
}
