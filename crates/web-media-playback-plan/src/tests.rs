use audio_core::{AudioDecodeCapabilitySnapshot, AudioDecodeCodecFamily};
use codec_core::VideoCodec;
use demux_api::{DemuxInputCapabilities, DemuxInputCapability};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ContainerFamily, DynamicRange,
    ExactSelectionIdentity, ExtractionGeneration, HttpScheme, PreferredHeightPolicy,
    PreferredVideoHeight, SelectionRequest, SemanticIdentity, StreamLayout, StreamLayoutKind,
    TransportFamily, VideoComponentDescriptor,
};

use crate::{
    CandidateCapabilityRejection, CandidatePolicyRejection, CandidateQualityScore,
    CandidateRejectionReason, CandidateRuntimeRequirements, DemuxCapabilityRegistration,
    DemuxCapabilityRejection, DemuxCapabilitySnapshot, HdrSelectionPolicy, PlanningCandidate,
    PlanningCandidateBuildError, PlaybackCapabilitySnapshot, PlaybackComponent,
    PlaybackPlanningError, TransportCapabilityRegistration, TransportCapabilitySnapshot,
    plan_playback, rank_playable_opaque_alternatives,
};

#[path = "tests/audio_containers.rs"]
mod audio_containers;
#[path = "tests/s42_profile.rs"]
mod s42_profile;
#[path = "tests/support.rs"]
mod support;

use support::*;

/// Один muxed candidate сообщает transport/demux/video/audio как разные missing layers.
#[test]
fn absent_transport_demux_video_and_audio_are_exact_typed_rejections() {
    let candidate = muxed_candidate("muxed", "muxed-semantic");
    let request = exact_request(&candidate);
    let snapshot = candidate_snapshot(vec![candidate]);
    let transport = TransportCapabilitySnapshot::default();
    let demux = DemuxCapabilitySnapshot::default();
    let video = empty_video_capabilities();
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::WebM],
    );

    let PlaybackPlanningError::ExactCandidateNotPlayable(rejection) =
        plan_playback(&snapshot, capabilities, &request, &policy)
            .expect_err("all four capability layers отсутствуют")
    else {
        panic!("ожидался exact candidate rejection");
    };

    assert_eq!(rejection.reasons().len(), 4);
    assert!(rejection.reasons().iter().any(|reason| matches!(
        reason,
        CandidateRejectionReason::Capability(CandidateCapabilityRejection::Transport(_))
    )));
    assert!(rejection.reasons().iter().any(|reason| matches!(
        reason,
        CandidateRejectionReason::Capability(CandidateCapabilityRejection::Demux(
            DemuxCapabilityRejection::ContainerUnavailable { .. }
        ))
    )));
    assert!(rejection.reasons().iter().any(|reason| matches!(
        reason,
        CandidateRejectionReason::Capability(CandidateCapabilityRejection::Video { .. })
    )));
    assert!(rejection.reasons().iter().any(|reason| matches!(
        reason,
        CandidateRejectionReason::Capability(CandidateCapabilityRejection::AudioUnavailable {
            component: PlaybackComponent::Muxed,
            family: AudioDecodeCodecFamily::Opus,
        })
    )));

    let failure = PlaybackPlanningError::ExactCandidateNotPlayable(rejection);
    let summary = failure.safe_summary();
    assert_eq!(summary.rejected_candidates(), 1);
    assert_eq!(summary.transport_rejections(), 1);
    assert_eq!(summary.demux_rejections(), 1);
    assert_eq!(summary.video_rejections(), 1);
    assert_eq!(summary.audio_rejections(), 1);
    assert_eq!(summary.policy_rejections(), 0);
    assert_eq!(
        summary.to_string(),
        "rejected_candidates=1, transport=1, demux=1, video=1, audio=1, policy=0"
    );
}

/// Muxed/separate/video-only/audio-only проходят один pure planning boundary.
#[test]
fn all_four_layout_shapes_are_playable_without_runtime_construction() {
    let (transport, demux) = full_resource_capabilities();
    let video = video_capabilities(vec![supported_video_format(VideoCodec::Vp9, false)]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty().with_available_family(AudioDecodeCodecFamily::Opus),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::WebM],
    );
    let cases = [
        (muxed_candidate("muxed", "a"), StreamLayoutKind::Muxed),
        (
            separate_candidate("separate", "b"),
            StreamLayoutKind::Separate,
        ),
        (
            video_only_candidate(VideoCandidateSpec {
                format_id: "video",
                semantic_key: "c",
                transport_raw: "https",
                container_raw: "webm",
                codec_raw: "vp09.00.41.08",
                height: 1080,
                dynamic_range: DynamicRange::Sdr,
                requirement: sdr_requirement(VideoCodec::Vp9, 1080),
                quality_score: 10,
            }),
            StreamLayoutKind::VideoOnly,
        ),
        (
            audio_only_candidate("audio", "d"),
            StreamLayoutKind::AudioOnly,
        ),
    ];

    for (candidate, expected_layout) in cases {
        let request = exact_request(&candidate);
        let snapshot = candidate_snapshot(vec![candidate]);
        let outcome = plan_playback(&snapshot, capabilities, &request, &policy)
            .expect("layout должен быть playable");
        assert_eq!(outcome.selected().layout(), expected_layout);
    }
}

/// BestPlayable не должен выбирать silent video из-за более высокого codec priority.
#[test]
fn best_playable_prefers_complete_av_over_preferred_video_only_codec() {
    let (transport, demux) = full_resource_capabilities();
    let video = video_capabilities(vec![
        supported_video_format(VideoCodec::H264, false),
        supported_video_format(VideoCodec::Vp9, false),
    ]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty().with_available_family(AudioDecodeCodecFamily::Opus),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::H264, VideoCodec::Vp9],
        vec![ContainerFamily::WebM],
    );
    let preferred_silent_video = video_only_candidate(VideoCandidateSpec {
        format_id: "preferred-silent-video",
        semantic_key: "preferred-silent-video",
        transport_raw: "https",
        container_raw: "webm",
        codec_raw: "avc1.640028",
        height: 2160,
        dynamic_range: DynamicRange::Sdr,
        requirement: sdr_requirement(VideoCodec::H264, 2160),
        quality_score: 1_000,
    });
    let complete_av = separate_candidate("complete-av", "complete-av");
    let snapshot = candidate_snapshot(vec![preferred_silent_video, complete_av]);

    let outcome = plan_playback(
        &snapshot,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("playable A/V candidate должен победить silent video");

    assert_eq!(outcome.selected().layout(), StreamLayoutKind::Separate);
}

/// Declared A/V content probe остаётся полноценным A/V независимо от более выгодных single-track tie-breakers.
#[test]
fn best_playable_prefers_declared_av_content_probe_over_video_only_and_audio_only() {
    let (transport, demux) = s42_resource_capabilities();
    let video = video_capabilities(vec![
        supported_video_format(VideoCodec::Vp9, false),
        supported_video_format(VideoCodec::H264, false),
    ]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty()
            .with_available_family(AudioDecodeCodecFamily::Opus)
            .with_available_family(AudioDecodeCodecFamily::Aac),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9, VideoCodec::H264],
        vec![ContainerFamily::WebM, ContainerFamily::F4f],
    );
    let declared_av = content_probed_hds_declared_av_candidate("declared-av", "declared-av", 1);
    let preferred_video_only = video_only_candidate(VideoCandidateSpec {
        format_id: "preferred-video-only",
        semantic_key: "preferred-video-only",
        transport_raw: "https",
        container_raw: "webm",
        codec_raw: "vp09.00.41.08",
        height: 2_160,
        dynamic_range: DynamicRange::Sdr,
        requirement: sdr_requirement(VideoCodec::Vp9, 2_160),
        quality_score: 10_000,
    });
    let preferred_audio_only = audio_only_candidate("preferred-audio-only", "preferred-audio-only");
    let snapshot = candidate_snapshot(vec![
        preferred_video_only,
        preferred_audio_only,
        declared_av,
    ]);

    let outcome = plan_playback(
        &snapshot,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("declared A/V content-probed candidate должен победить single-track alternatives");

    assert_eq!(outcome.selected().semantic_identity().key(), "declared-av");
    assert_eq!(outcome.selected().layout(), StreamLayoutKind::ContentProbed);
}

/// Unknown content metadata не получает выдуманную A/V полноту и уступает preferred single-track row.
#[test]
fn best_playable_does_not_promote_unknown_content_probe_to_complete_av() {
    let (transport, demux) = s42_resource_capabilities();
    let video = video_capabilities(vec![supported_video_format(VideoCodec::Vp9, false)]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::WebM, ContainerFamily::Ogg],
    );
    let unknown_content = content_probed_ogg_candidate_with_hints(
        "unknown-content",
        "unknown-content",
        None,
        DynamicRange::Unknown,
        1,
    );
    let preferred_video_only = video_only_candidate(VideoCandidateSpec {
        format_id: "preferred-video-only",
        semantic_key: "preferred-video-only",
        transport_raw: "https",
        container_raw: "webm",
        codec_raw: "vp09.00.41.08",
        height: 1_080,
        dynamic_range: DynamicRange::Sdr,
        requirement: sdr_requirement(VideoCodec::Vp9, 1_080),
        quality_score: 10_000,
    });
    let snapshot = candidate_snapshot(vec![unknown_content, preferred_video_only]);

    let outcome = plan_playback(
        &snapshot,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("unknown content probe и video-only candidate должны оставаться playable");

    assert_eq!(
        outcome.selected().semantic_identity().key(),
        "preferred-video-only"
    );
    assert_eq!(outcome.selected().layout(), StreamLayoutKind::VideoOnly);
}

#[test]
fn grouped_opaque_ranking_is_stable_when_source_order_reverses() {
    let (transport, demux) = full_resource_capabilities();
    let video = video_capabilities(vec![supported_video_format(VideoCodec::Vp9, false)]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::WebM],
    );
    let candidate = |format_id: &str, semantic_key: &str, quality_score| {
        video_only_candidate(VideoCandidateSpec {
            format_id,
            semantic_key,
            transport_raw: "https",
            container_raw: "webm",
            codec_raw: "vp09.00.41.08",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::Vp9, 1080),
            quality_score,
        })
    };
    let better = candidate("better", "better", 100);
    let worse = candidate("worse", "worse", 10);
    let better_selection = ExactSelectionIdentity::new(
        better.descriptor().identity().clone(),
        better.descriptor().semantic_identity().clone(),
    )
    .unwrap();
    let worse_selection = ExactSelectionIdentity::new(
        worse.descriptor().identity().clone(),
        worse.descriptor().semantic_identity().clone(),
    )
    .unwrap();

    for snapshot in [
        candidate_snapshot(vec![worse.clone(), better.clone()]),
        candidate_snapshot(vec![better, worse]),
    ] {
        assert_eq!(snapshot.candidates().len(), 2);
        let ranking = rank_playable_opaque_alternatives(&snapshot, capabilities, &policy)
            .expect("оба opaque alternatives playable");
        assert_eq!(ranking.rank_of(&better_selection), Some(0));
        assert_eq!(ranking.rank_of(&worse_selection), Some(1));
        let ranked_identities = ranking.ranked_candidate_identities().collect::<Vec<_>>();
        assert_eq!(
            ranked_identities,
            [
                (better_selection.exact(), better_selection.semantic()),
                (worse_selection.exact(), worse_selection.semantic())
            ]
        );
    }
}

/// Preferred height выбирает exact, затем closest lower, затем closest higher.
#[test]
fn preferred_height_uses_exact_lower_and_higher_buckets() {
    let (transport, demux) = full_resource_capabilities();
    let video = video_capabilities(vec![supported_video_format(VideoCodec::Vp9, false)]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::Prefer(
            PreferredVideoHeight::new(2160).expect("2160 preference валидна"),
        ),
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::WebM],
    );
    let candidate = |id: &str, height: u32, quality| {
        video_only_candidate(VideoCandidateSpec {
            format_id: id,
            semantic_key: id,
            transport_raw: "https",
            container_raw: "webm",
            codec_raw: "vp09.00.41.08",
            height,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::Vp9, height),
            quality_score: quality,
        })
    };

    let exact = candidate_snapshot(vec![
        candidate("lower", 1440, 100),
        candidate("exact", 2160, 1),
        candidate("higher", 2880, 200),
    ]);
    let selected = plan_playback(
        &exact,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("exact height playable");
    assert_eq!(selected.selected().semantic_identity().key(), "exact");

    let lower = candidate_snapshot(vec![
        candidate("lower-1080", 1080, 100),
        candidate("lower-1440", 1440, 1),
        candidate("higher-2880", 2880, 200),
    ]);
    let selected = plan_playback(
        &lower,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("lower fallback playable");
    assert_eq!(selected.selected().semantic_identity().key(), "lower-1440");

    let higher = candidate_snapshot(vec![
        candidate("higher-4320", 4320, 100),
        candidate("higher-2880", 2880, 1),
    ]);
    let selected = plan_playback(
        &higher,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("higher fallback playable");
    assert_eq!(selected.selected().semantic_identity().key(), "higher-2880");
}

/// HDR, codec и container остаются отдельными deterministic tie-break уровнями.
#[test]
fn hdr_codec_and_container_tie_breaks_follow_explicit_policy() {
    let (transport, demux) = full_resource_capabilities();
    let video = video_capabilities(vec![
        supported_video_format(VideoCodec::Vp9, false),
        supported_video_format(VideoCodec::Vp9, true),
        supported_video_format(VideoCodec::H264, false),
    ]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );

    let hdr_policy = selection_policy(
        HdrSelectionPolicy::PreferHdrWhenAvailable,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9, VideoCodec::H264],
        vec![ContainerFamily::WebM, ContainerFamily::Matroska],
    );
    let hdr_snapshot = candidate_snapshot(vec![
        video_only_candidate(VideoCandidateSpec {
            format_id: "sdr",
            semantic_key: "sdr",
            transport_raw: "https",
            container_raw: "webm",
            codec_raw: "vp09.00.41.08",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::Vp9, 1080),
            quality_score: 100,
        }),
        video_only_candidate(VideoCandidateSpec {
            format_id: "hdr",
            semantic_key: "hdr",
            transport_raw: "https",
            container_raw: "webm",
            codec_raw: "vp09.02.41.10",
            height: 1080,
            dynamic_range: DynamicRange::Hdr,
            requirement: hdr_requirement(1080),
            quality_score: 1,
        }),
    ]);
    assert_eq!(
        plan_playback(
            &hdr_snapshot,
            capabilities,
            &SelectionRequest::BestPlayable,
            &hdr_policy,
        )
        .expect("HDR candidate playable")
        .selected()
        .semantic_identity()
        .key(),
        "hdr"
    );

    let codec_policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::H264, VideoCodec::Vp9],
        vec![ContainerFamily::WebM, ContainerFamily::Matroska],
    );
    let codec_snapshot = candidate_snapshot(vec![
        video_only_candidate(VideoCandidateSpec {
            format_id: "vp9",
            semantic_key: "vp9",
            transport_raw: "https",
            container_raw: "webm",
            codec_raw: "vp09.00.41.08",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::Vp9, 1080),
            quality_score: 100,
        }),
        video_only_candidate(VideoCandidateSpec {
            format_id: "h264",
            semantic_key: "h264",
            transport_raw: "https",
            container_raw: "matroska",
            codec_raw: "avc1.640028",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::H264, 1080),
            quality_score: 1,
        }),
    ]);
    assert_eq!(
        plan_playback(
            &codec_snapshot,
            capabilities,
            &SelectionRequest::BestPlayable,
            &codec_policy,
        )
        .expect("H.264 candidate playable")
        .selected()
        .semantic_identity()
        .key(),
        "h264"
    );

    let container_policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::Matroska, ContainerFamily::WebM],
    );
    let container_snapshot = candidate_snapshot(vec![
        video_only_candidate(VideoCandidateSpec {
            format_id: "webm",
            semantic_key: "webm",
            transport_raw: "https",
            container_raw: "webm",
            codec_raw: "vp09.00.41.08",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::Vp9, 1080),
            quality_score: 100,
        }),
        video_only_candidate(VideoCandidateSpec {
            format_id: "matroska",
            semantic_key: "matroska",
            transport_raw: "https",
            container_raw: "matroska",
            codec_raw: "vp09.00.41.08",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::Vp9, 1080),
            quality_score: 1,
        }),
    ]);
    assert_eq!(
        plan_playback(
            &container_snapshot,
            capabilities,
            &SelectionRequest::BestPlayable,
            &container_policy,
        )
        .expect("Matroska candidate playable")
        .selected()
        .semantic_identity()
        .key(),
        "matroska"
    );
}

/// Incompatible HLS candidate сохраняет rejection, но не блокирует HTTP candidate.
#[test]
fn one_incompatible_candidate_does_not_block_another() {
    let (transport, demux) = full_resource_capabilities();
    let video = video_capabilities(vec![supported_video_format(VideoCodec::Vp9, false)]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::WebM],
    );
    let snapshot = candidate_snapshot(vec![
        video_only_candidate(VideoCandidateSpec {
            format_id: "hls",
            semantic_key: "hls",
            transport_raw: "m3u8_native",
            container_raw: "webm",
            codec_raw: "vp09.00.41.08",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::Vp9, 1080),
            quality_score: 100,
        }),
        video_only_candidate(VideoCandidateSpec {
            format_id: "http",
            semantic_key: "http",
            transport_raw: "https",
            container_raw: "webm",
            codec_raw: "vp09.00.41.08",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::Vp9, 1080),
            quality_score: 1,
        }),
    ]);

    let outcome = plan_playback(
        &snapshot,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("HTTP candidate должен пройти");
    assert_eq!(outcome.selected().semantic_identity().key(), "http");
    assert_eq!(outcome.rejected_candidates().len(), 1);
    assert!(
        outcome.rejected_candidates()[0]
            .reasons()
            .iter()
            .any(|reason| matches!(
                reason,
                CandidateRejectionReason::Capability(CandidateCapabilityRejection::Transport(_))
            ))
    );
}

/// Exact различает stale generation и reuse ID с изменившейся semantic identity.
#[test]
fn exact_rejects_stale_generation_and_changed_semantic_identity() {
    let candidate = audio_only_candidate("same-id", "current-semantic");
    let snapshot = candidate_snapshot(vec![candidate.clone()]);
    let (transport, demux) = full_resource_capabilities();
    let video = empty_video_capabilities();
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty().with_available_family(AudioDecodeCodecFamily::Opus),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        Vec::new(),
        vec![ContainerFamily::WebM],
    );
    let stale = SelectionRequest::Exact(
        ExactSelectionIdentity::new(
            CandidateIdentity::new(
                TEST_SOURCE,
                ExtractionGeneration::new(10),
                CandidateFormatIdentity::new("same-id").expect("format identity валидна"),
            ),
            SemanticIdentity::new(TEST_SOURCE, "current-semantic")
                .expect("semantic identity валидна"),
        )
        .expect("stale exact request structurally валиден"),
    );
    assert!(matches!(
        plan_playback(&snapshot, capabilities, &stale, &policy),
        Err(PlaybackPlanningError::StaleExactIdentity { .. })
    ));

    let changed_semantic = SelectionRequest::Exact(
        ExactSelectionIdentity::new(
            candidate.descriptor().identity().clone(),
            SemanticIdentity::new(TEST_SOURCE, "old-semantic").expect("semantic identity валидна"),
        )
        .expect("exact request structurally валиден"),
    );
    assert_eq!(
        plan_playback(&snapshot, capabilities, &changed_semantic, &policy),
        Err(PlaybackPlanningError::ExactSemanticIdentityChanged)
    );
}

/// Static unknown transport отсекается admission-ом и не маскируется runtime failure.
#[test]
fn static_incompatibility_cannot_enter_runtime_planner() {
    let component = VideoComponentDescriptor::new(
        transport("future_transport"),
        container("webm"),
        video_track("vp09.00.41.08", 1080, DynamicRange::Sdr),
    );
    let descriptor = candidate_descriptor(
        "static-rejected",
        "static-rejected",
        StreamLayout::VideoOnly(component),
    );

    assert!(matches!(
        PlanningCandidate::new(
            descriptor,
            CandidateRuntimeRequirements::VideoOnly {
                video: sdr_requirement(VideoCodec::Vp9, 1080),
            },
            CandidateQualityScore::new(1),
        ),
        Err(PlanningCandidateBuildError::StaticTransportRejected { .. })
    ));
}

/// Candidate admission не позволяет занизить resolution в video capability query.
#[test]
fn descriptor_resolution_must_match_video_requirement() {
    let component = VideoComponentDescriptor::new(
        transport("https"),
        container("webm"),
        video_track("vp09.00.41.08", 2160, DynamicRange::Sdr),
    );
    let descriptor = candidate_descriptor(
        "resolution-mismatch",
        "resolution-mismatch",
        StreamLayout::VideoOnly(component),
    );

    assert_eq!(
        PlanningCandidate::new(
            descriptor,
            CandidateRuntimeRequirements::VideoOnly {
                video: sdr_requirement(VideoCodec::Vp9, 1080),
            },
            CandidateQualityScore::new(1),
        ),
        Err(PlanningCandidateBuildError::VideoResolutionMismatch)
    );
}

/// Existing transport и demux без общей input shape дают отдельный typed отказ.
#[test]
fn transport_demux_input_shape_mismatch_is_not_reported_as_absence() {
    let candidate = audio_only_candidate("input-mismatch", "input-mismatch");
    let request = exact_request(&candidate);
    let snapshot = candidate_snapshot(vec![candidate]);
    let transport = TransportCapabilitySnapshot::new(vec![
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveHttp(HttpScheme::Https),
            DemuxInputCapabilities::only(DemuxInputCapability::StreamingBytes),
        )
        .expect("transport registration валидна"),
    ]);
    let demux = DemuxCapabilitySnapshot::new(vec![
        DemuxCapabilityRegistration::new(
            ContainerFamily::WebM,
            DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes),
        )
        .expect("demux registration валидна"),
    ]);
    let video = empty_video_capabilities();
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty().with_available_family(AudioDecodeCodecFamily::Opus),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        Vec::new(),
        vec![ContainerFamily::WebM],
    );

    let PlaybackPlanningError::ExactCandidateNotPlayable(rejection) =
        plan_playback(&snapshot, capabilities, &request, &policy)
            .expect_err("transport/demux shapes не пересекаются")
    else {
        panic!("ожидался exact demux rejection");
    };
    assert!(rejection.reasons().iter().any(|reason| matches!(
        reason,
        CandidateRejectionReason::Capability(CandidateCapabilityRejection::Demux(
            DemuxCapabilityRejection::InputShapeMismatch { .. }
        ))
    )));
}

/// Deferred HLS playable при HLS transport и TS/fMP4 demux intersection.
#[test]
fn hls_deferred_is_playable_with_hls_transport_and_ts_or_fmp4_demux() {
    let candidate = hls_deferred_candidate("hls-deferred", "hls-deferred", 720, 720_000_000);
    let request = exact_request(&candidate);
    let snapshot = candidate_snapshot(vec![candidate]);
    let (transport, demux) = s42_resource_capabilities();
    let video = empty_video_capabilities();
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        Vec::new(),
        vec![ContainerFamily::MpegTs, ContainerFamily::FragmentedIsoBmff],
    );

    let outcome = plan_playback(&snapshot, capabilities, &request, &policy)
        .expect("deferred HLS candidate playable");
    assert_eq!(
        outcome.selected().layout(),
        StreamLayoutKind::HlsMuxedCodecDeferred
    );
}

/// Content-probed Ogg проверяет transport/demux сейчас, а неизвестные codecs — после open.
#[test]
fn content_probed_ogg_is_playable_without_invented_decode_requirements() {
    let candidate = content_probed_ogg_candidate("ogg-probed", "ogg-probed");
    let request = exact_request(&candidate);
    let snapshot = candidate_snapshot(vec![candidate]);
    let transport = TransportCapabilitySnapshot::new(vec![
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveHttp(HttpScheme::Https),
            DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes),
        )
        .expect("HTTP transport registration валидна"),
    ]);
    let demux = DemuxCapabilitySnapshot::new(vec![
        DemuxCapabilityRegistration::new(
            ContainerFamily::Ogg,
            DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes),
        )
        .expect("Ogg demux registration валидна"),
    ]);
    let video = empty_video_capabilities();
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        Vec::new(),
        vec![ContainerFamily::Ogg],
    );

    let outcome = plan_playback(&snapshot, capabilities, &request, &policy)
        .expect("content-probed Ogg должен пройти static planner layers");
    assert_eq!(outcome.selected().layout(), StreamLayoutKind::ContentProbed);
    assert_eq!(outcome.selected().matched_video_output(), None);
}

/// Soft content hints участвуют в ranking, но не становятся hard HDR capability evidence.
#[test]
fn content_probed_height_hint_ranks_without_bypassing_runtime_hdr_proof() {
    let snapshot = candidate_snapshot(vec![
        content_probed_ogg_candidate_with_hints(
            "probed-480",
            "probed-480",
            Some(480),
            DynamicRange::Hdr,
            300,
        ),
        content_probed_ogg_candidate_with_hints(
            "probed-720",
            "probed-720",
            Some(720),
            DynamicRange::Hdr,
            1,
        ),
        content_probed_ogg_candidate_with_hints(
            "probed-1080",
            "probed-1080",
            Some(1_080),
            DynamicRange::Hdr,
            500,
        ),
    ]);
    let transport = TransportCapabilitySnapshot::new(vec![
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveHttp(HttpScheme::Https),
            DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes),
        )
        .unwrap(),
    ]);
    let demux = DemuxCapabilitySnapshot::new(vec![
        DemuxCapabilityRegistration::new(
            ContainerFamily::Ogg,
            DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes),
        )
        .unwrap(),
    ]);
    let video = empty_video_capabilities();
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::Prefer(PreferredVideoHeight::new(720).unwrap()),
        Vec::new(),
        vec![ContainerFamily::Ogg],
    );

    let outcome = plan_playback(
        &snapshot,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("soft HDR hint не должен подменять runtime proof");

    assert_eq!(outcome.selected().semantic_identity().key(), "probed-720");
}

/// Content probe не обходит отсутствие зарегистрированного demux container path-а.
#[test]
fn content_probed_ogg_rejects_missing_ogg_demux() {
    let candidate = content_probed_ogg_candidate("ogg-probed", "ogg-probed");
    let request = exact_request(&candidate);
    let snapshot = candidate_snapshot(vec![candidate]);
    let transport = TransportCapabilitySnapshot::new(vec![
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveHttp(HttpScheme::Https),
            DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes),
        )
        .expect("HTTP transport registration валидна"),
    ]);
    let demux = DemuxCapabilitySnapshot::default();
    let video = empty_video_capabilities();
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        Vec::new(),
        vec![ContainerFamily::Ogg],
    );

    let PlaybackPlanningError::ExactCandidateNotPlayable(rejection) =
        plan_playback(&snapshot, capabilities, &request, &policy)
            .expect_err("без Ogg demux content-probed candidate должен быть отклонён")
    else {
        panic!("ожидался exact content-probed rejection");
    };
    assert!(rejection.reasons().iter().any(|reason| matches!(
        reason,
        CandidateRejectionReason::Capability(CandidateCapabilityRejection::Demux(
            DemuxCapabilityRejection::ContainerUnavailable {
                component: PlaybackComponent::ContentProbed,
                container: ContainerFamily::Ogg,
            }
        ))
    )));
}

/// Deferred HLS отклоняется без HLS transport capability.
#[test]
fn hls_deferred_rejects_without_hls_transport() {
    let candidate = hls_deferred_candidate("hls-deferred", "hls-deferred", 720, 1);
    let request = exact_request(&candidate);
    let snapshot = candidate_snapshot(vec![candidate]);
    let transport = TransportCapabilitySnapshot::new(vec![
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveHttp(HttpScheme::Https),
            DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes),
        )
        .expect("progressive transport registration валидна"),
    ]);
    let demux = DemuxCapabilitySnapshot::new(vec![
        DemuxCapabilityRegistration::new(
            ContainerFamily::MpegTs,
            DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments),
        )
        .expect("TS demux registration валидна"),
    ]);
    let video = empty_video_capabilities();
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        Vec::new(),
        vec![ContainerFamily::MpegTs],
    );

    let PlaybackPlanningError::ExactCandidateNotPlayable(rejection) =
        plan_playback(&snapshot, capabilities, &request, &policy)
            .expect_err("deferred HLS без transport capability отклонён")
    else {
        panic!("ожидался exact transport rejection");
    };
    assert!(rejection.reasons().iter().any(|reason| matches!(
        reason,
        CandidateRejectionReason::Capability(CandidateCapabilityRejection::Transport(_))
    )));
}

/// BestPlayable ранжирует deferred HLS по height через quality_score.
#[test]
fn best_playable_ranks_deferred_hls_by_height() {
    let low = hls_deferred_candidate("hls-480", "hls-480", 480, 480_000_000);
    let high = hls_deferred_candidate("hls-1080", "hls-1080", 1080, 1_080_000_000);
    let snapshot = candidate_snapshot(vec![low, high]);
    let (transport, demux) = s42_resource_capabilities();
    let video = empty_video_capabilities();
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        Vec::new(),
        vec![ContainerFamily::MpegTs, ContainerFamily::FragmentedIsoBmff],
    );

    let outcome = plan_playback(
        &snapshot,
        capabilities,
        &SelectionRequest::BestPlayable,
        &policy,
    )
    .expect("deferred HLS candidates playable");
    assert_eq!(
        outcome.selected().layout(),
        StreamLayoutKind::HlsMuxedCodecDeferred
    );
    assert_eq!(
        outcome.selected().exact_identity().format().as_str(),
        "hls-1080"
    );
}

/// Unknown dynamic range остаётся explicit policy rejection после capability checks.
#[test]
fn unknown_dynamic_range_is_not_guessed_as_sdr() {
    let candidate = video_only_candidate(VideoCandidateSpec {
        format_id: "unknown-range",
        semantic_key: "unknown-range",
        transport_raw: "https",
        container_raw: "webm",
        codec_raw: "vp09.00.41.08",
        height: 1080,
        dynamic_range: DynamicRange::Unknown,
        requirement: sdr_requirement(VideoCodec::Vp9, 1080),
        quality_score: 1,
    });
    let request = exact_request(&candidate);
    let snapshot = candidate_snapshot(vec![candidate]);
    let (transport, demux) = full_resource_capabilities();
    let video = video_capabilities(vec![supported_video_format(VideoCodec::Vp9, false)]);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &transport,
        &demux,
        &video,
        AudioDecodeCapabilitySnapshot::empty(),
    );
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Vp9],
        vec![ContainerFamily::WebM],
    );

    let PlaybackPlanningError::ExactCandidateNotPlayable(rejection) =
        plan_playback(&snapshot, capabilities, &request, &policy)
            .expect_err("unknown range нельзя угадать как SDR")
    else {
        panic!("ожидался policy rejection");
    };
    assert!(rejection.reasons().iter().any(|reason| matches!(
        reason,
        CandidateRejectionReason::Policy(CandidatePolicyRejection::UnknownDynamicRange)
    )));
}
