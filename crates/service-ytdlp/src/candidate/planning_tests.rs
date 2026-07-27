use codec_core::{
    BitDepth, ChromaSubsampling, ColorMetadataConfidence, ColorMetadataOrigin, ColorPrimaries,
    ColorRange, H264Profile, MatrixCoefficients, TransferFunction, VideoDecodeRequirement,
    VideoProfile, Vp9Profile,
};
use serde_json::{Value, json};
use web_media_core::{ExtractionGeneration, SourceIdentity, StreamLayout};
use web_media_playback_plan::{CandidateRuntimeRequirements, PlanningCandidate};

use super::raw::YtDlpCandidateDocument;

/// Создаёт один progressive muxed format без secret request material.
fn progressive_muxed_format(
    format_id: &str,
    extension: &str,
    container: &str,
    video_codec: &str,
    audio_codec: &str,
) -> Value {
    json!({
        "format_id": format_id,
        "url": format!("https://media.invalid/{format_id}"),
        "protocol": "https",
        "ext": extension,
        "container": container,
        "vcodec": video_codec,
        "acodec": audio_codec,
        "dynamic_range": "SDR"
    })
}

/// Возвращает video requirement только для ожидаемого muxed planning candidate-а.
fn muxed_video_requirement(candidate: &PlanningCandidate) -> &VideoDecodeRequirement {
    match candidate.runtime_requirements() {
        CandidateRuntimeRequirements::Muxed { video, .. } => video,
        unexpected => panic!("ожидался muxed runtime requirement, получен {unexpected:?}"),
    }
}

/// Возвращает исходный codec tag, чтобы test не полагался только на порядок inventory.
fn muxed_video_codec_tag(candidate: &PlanningCandidate) -> &str {
    match candidate.descriptor().layout() {
        StreamLayout::Muxed(component) => component.video().codec().raw().as_str(),
        unexpected => panic!("ожидался muxed descriptor, получен {unexpected:?}"),
    }
}

/// Возвращает video requirement независимо от single-component layout-а.
fn video_requirement(candidate: &PlanningCandidate) -> &VideoDecodeRequirement {
    match candidate.runtime_requirements() {
        CandidateRuntimeRequirements::Muxed { video, .. }
        | CandidateRuntimeRequirements::Separate { video, .. }
        | CandidateRuntimeRequirements::VideoOnly { video } => video,
        unexpected => panic!("ожидался video runtime requirement, получен {unexpected:?}"),
    }
}

/// Ordinary Baseline и Constrained Baseline остаются разными профилями и не теряют соседа.
#[test]
fn h264_baseline_indications_keep_full_planning_inventory_and_exact_requirements() {
    let document: YtDlpCandidateDocument = serde_json::from_value(json!({
        "formats": [
            progressive_muxed_format("vp9-neighbor", "webm", "webm", "vp9", "opus"),
            progressive_muxed_format(
                "h264-baseline",
                "mp4",
                "mp4",
                "avc1.42001E",
                "mp4a.40.2"
            ),
            progressive_muxed_format(
                "h264-constrained-baseline",
                "mp4",
                "mp4",
                "avc1.42E01E",
                "mp4a.40.2"
            )
        ]
    }))
    .expect("synthetic yt-dlp document должен десериализоваться");
    let snapshot = super::normalize_candidate_document(
        document,
        SourceIdentity::new(71),
        ExtractionGeneration::new(11),
    );

    let planning_snapshot = snapshot
        .planning_snapshot()
        .expect("оба H.264 Baseline profile и playable сосед должны планироваться");
    assert_eq!(planning_snapshot.candidates().len(), 3);

    let baseline_candidate = &planning_snapshot.candidates()[1];
    assert_eq!(muxed_video_codec_tag(baseline_candidate), "avc1.42001E");
    let baseline = muxed_video_requirement(baseline_candidate);
    assert_eq!(
        baseline.profile,
        Some(VideoProfile::H264(H264Profile::Baseline))
    );
    assert_eq!(baseline.bit_depth, Some(BitDepth::Eight));
    assert_eq!(baseline.chroma, Some(ChromaSubsampling::Yuv420));

    let constrained_baseline_candidate = &planning_snapshot.candidates()[2];
    assert_eq!(
        muxed_video_codec_tag(constrained_baseline_candidate),
        "avc1.42E01E"
    );
    let constrained_baseline = muxed_video_requirement(constrained_baseline_candidate);
    assert_eq!(
        constrained_baseline.profile,
        Some(VideoProfile::H264(H264Profile::ConstrainedBaseline))
    );
    assert_eq!(constrained_baseline.bit_depth, Some(BitDepth::Eight));
    assert_eq!(constrained_baseline.chroma, Some(ChromaSubsampling::Yuv420));
}

/// YouTube HDR shorthand не должен обрушать весь snapshot с playable SDR соседями.
#[test]
fn youtube_vp9_profile2_hdr_shorthand_maps_to_complete_runtime_requirement() {
    let document: YtDlpCandidateDocument = serde_json::from_value(json!({
        "formats": [
            progressive_muxed_format("sdr-neighbor", "webm", "webm", "vp9", "opus"),
            {
                "format_id": "330",
                "url": "https://media.invalid/330",
                "protocol": "https",
                "ext": "webm",
                "container": "webm_dash",
                "vcodec": "vp9.2",
                "acodec": "none",
                "width": 256,
                "height": 144,
                "fps": 60,
                "dynamic_range": "HDR10"
            }
        ]
    }))
    .expect("synthetic yt-dlp document должен десериализоваться");
    let snapshot = super::normalize_candidate_document(
        document,
        SourceIdentity::new(72),
        ExtractionGeneration::new(12),
    );

    let planning_snapshot = snapshot
        .planning_snapshot()
        .expect("yt-dlp vp9.2 HDR и playable SDR сосед должны планироваться");
    assert_eq!(planning_snapshot.candidates().len(), 2);

    let hdr = video_requirement(&planning_snapshot.candidates()[1]);
    assert_eq!(hdr.profile, Some(VideoProfile::Vp9(Vp9Profile::Profile2)));
    assert_eq!(hdr.bit_depth, Some(BitDepth::Ten));
    assert_eq!(hdr.chroma, Some(ChromaSubsampling::Yuv420));
    assert!(hdr.requires_hdr_processing());
    let color = hdr
        .color
        .as_ref()
        .expect("HDR10 должен дать strict color evidence");
    assert_eq!(color.range, ColorRange::Limited);
    assert_eq!(color.matrix, MatrixCoefficients::Bt2020);
    assert_eq!(color.primaries, ColorPrimaries::Bt2020);
    assert_eq!(color.transfer, TransferFunction::Pq);
    assert_eq!(color.origin, ColorMetadataOrigin::Manifest);
    assert_eq!(color.confidence, ColorMetadataConfidence::Hint);
}

/// Общий HDR label не угадывает PQ/HLG и остаётся capability-unplayable.
#[test]
fn ambiguous_hdr_label_does_not_create_color_evidence() {
    let mut hdr = progressive_muxed_format("ambiguous-hdr", "webm", "webm", "vp9.2", "opus");
    hdr["dynamic_range"] = json!("HDR");
    let document: YtDlpCandidateDocument = serde_json::from_value(json!({"formats": [hdr]}))
        .expect("synthetic yt-dlp document должен десериализоваться");
    let snapshot = super::normalize_candidate_document(
        document,
        SourceIdentity::new(73),
        ExtractionGeneration::new(13),
    );

    let planning = snapshot
        .planning_snapshot()
        .expect("ambiguous HDR остаётся typed planner candidate");
    let requirement = video_requirement(&planning.candidates()[0]);
    assert!(requirement.requires_hdr_processing());
    assert!(requirement.color.is_none());
}
