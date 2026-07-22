use codec_core::{BitDepth, ChromaSubsampling, H264Profile, VideoDecodeRequirement, VideoProfile};
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
