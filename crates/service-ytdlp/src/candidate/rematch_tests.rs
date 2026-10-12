use serde_json::{Value, json};
use web_media_core::{ExtractionGeneration, SourceIdentity, StreamLayoutKind};

use super::model::{
    YtDlpCandidateComponentRole, YtDlpCandidateMatchKind, YtDlpCandidateRematchError,
    YtDlpCandidateSnapshot, YtDlpNormalizedCandidate,
};
use super::normalize::normalize_candidate_document;
use super::raw::YtDlpCandidateDocument;

/// Нормализует synthetic document тем же production boundary, что process output.
fn snapshot(format: Value, generation: u64) -> YtDlpCandidateSnapshot {
    let document: YtDlpCandidateDocument = serde_json::from_value(json!({
        "formats": [format]
    }))
    .expect("synthetic yt-dlp document should be deserialized");
    normalize_candidate_document(
        document,
        SourceIdentity::new(81),
        ExtractionGeneration::new(generation),
    )
}

/// Возвращает единственный accepted candidate без fallback по selected row.
fn accepted(snapshot: &YtDlpCandidateSnapshot) -> &YtDlpNormalizedCandidate {
    let mut candidates = snapshot.accepted_inventory_candidates();
    let candidate = candidates.next().expect("The candidate must be accepted");
    assert!(candidates.next().is_none(), "One candidate was expected");
    candidate
}

/// Строит один physical Ogg resource с управляемой полнотой codec metadata.
fn ogg_format(format_id: &str, video_codec: Option<&str>, audio_codec: Option<&str>) -> Value {
    json!({
        "format_id": format_id,
        "url": "https://media.invalid/stable-audio.ogg",
        "protocol": "https",
        "ext": "ogg",
        "container": "ogg",
        "vcodec": video_codec,
        "acodec": audio_codec
    })
}

/// Fresh declared codec metadata уточняет тот же content-probed physical format.
#[test]
fn fresh_content_probe_rematch_accepts_unknown_to_declared_codec_refinement() {
    let original = snapshot(ogg_format("stable-ogg", None, None), 1);
    let selection = original
        .selection_for(accepted(&original))
        .expect("original candidate belong snapshot-at");

    let refreshed = snapshot(ogg_format("stable-ogg", Some("none"), Some("vorbis")), 2);
    assert_ne!(
        selection.semantic_identity(),
        accepted(&refreshed).descriptor().semantic_identity(),
        "compatibility fallback should not weaken the main semantic identity"
    );
    let matched = refreshed
        .rematch_exact(&selection)
        .expect("codec refinement of the same physical format must rematch.");

    assert_eq!(matched.kind(), YtDlpCandidateMatchKind::SemanticRematch);
    assert_eq!(
        matched.candidate().descriptor().layout().kind(),
        StreamLayoutKind::AudioOnly
    );
    assert_eq!(
        matched
            .candidate()
            .component_request_summaries()
            .map(|summary| summary.role)
            .collect::<Vec<_>>(),
        [YtDlpCandidateComponentRole::Audio]
    );

    let changed_inside_same_generation =
        snapshot(ogg_format("stable-ogg", Some("none"), Some("vorbis")), 1);
    assert_eq!(
        changed_inside_same_generation
            .rematch_exact(&selection)
            .expect_err("Exact identity should not accept changed attributes."),
        YtDlpCandidateRematchError::ExactAttributesChanged
    );
}

/// Потеря optional codec metadata не должна ломать controlled reopen того же format-а.
#[test]
fn fresh_content_probe_rematch_accepts_declared_to_unknown_codec_drift() {
    let original = snapshot(ogg_format("stable-ogg", Some("none"), Some("vorbis")), 3);
    let selection = original
        .selection_for(accepted(&original))
        .expect("original candidate belong snapshot-at");
    let refreshed = snapshot(ogg_format("stable-ogg", None, None), 4);

    let matched = refreshed
        .rematch_exact(&selection)
        .expect("metadata loss equally physical format must pass runtime re-proof");

    assert_eq!(matched.kind(), YtDlpCandidateMatchKind::SemanticRematch);
    assert_eq!(
        matched.candidate().descriptor().layout().kind(),
        StreamLayoutKind::ContentProbed
    );
    assert_eq!(
        matched
            .candidate()
            .component_request_summaries()
            .map(|summary| summary.role)
            .collect::<Vec<_>>(),
        [YtDlpCandidateComponentRole::ContentProbed]
    );
}

/// Content-probe wildcard не разрешает подменять выбранный physical format соседним.
#[test]
fn content_probe_rematch_rejects_different_physical_format_identity() {
    let original = snapshot(ogg_format("physical-a", None, None), 5);
    let selection = original
        .selection_for(accepted(&original))
        .expect("original candidate belong snapshot-at");
    let refreshed = snapshot(ogg_format("physical-b", Some("none"), Some("vorbis")), 6);

    assert_eq!(
        refreshed
            .rematch_exact(&selection)
            .expect_err("other format ID not the same physical resource"),
        YtDlpCandidateRematchError::StaleExactIdentity
    );

    let equally_unknown_neighbor = snapshot(ogg_format("physical-b", None, None), 7);
    assert_eq!(
        equally_unknown_neighbor
            .rematch_exact(&selection)
            .expect_err("weak equal content-probed layout does not cancel physical format anchor"),
        YtDlpCandidateRematchError::StaleExactIdentity
    );

    let declared_vorbis = snapshot(ogg_format("codec-conflict", None, Some("vorbis")), 8);
    let declared_selection = declared_vorbis
        .selection_for(accepted(&declared_vorbis))
        .expect("The declared content-probed candidate belongs to snapshot.");
    let declared_opus = snapshot(ogg_format("codec-conflict", None, Some("opus")), 9);
    assert_eq!(
        declared_opus
            .rematch_exact(&declared_selection)
            .expect_err("Two conflicting declared codecs are not metadata drift"),
        YtDlpCandidateRematchError::StaleExactIdentity
    );

    let color_original = snapshot(ogg_format("color-conflict", None, None), 10);
    let color_selection = color_original
        .selection_for(accepted(&color_original))
        .expect("color baseline belong snapshot-at");
    let mut hdr_format = ogg_format("color-conflict", None, None);
    hdr_format["dynamic_range"] = json!("HDR10");
    let color_changed = snapshot(hdr_format, 11);
    assert_eq!(
        color_changed
            .rematch_exact(&color_selection)
            .expect_err("color evidence is not optional codec drift"),
        YtDlpCandidateRematchError::StaleExactIdentity
    );
}
