//! Compound-кандидат yt-dlp (`requested_formats`) раскладывается обратно на строки inventory.

use serde_json::{Value, json};
use web_media_core::{ExtractionGeneration, SourceIdentity, StreamLayout};

use super::model::{YtDlpCandidateSelection, YtDlpCandidateSnapshot};
use super::normalize::normalize_candidate_document;
use super::raw::YtDlpCandidateDocument;

fn video_format(format_id: &str, url: &str, height: u32) -> Value {
    json!({
        "format_id": format_id, "url": url, "protocol": "https", "ext": "mp4",
        "container": "mp4_dash", "vcodec": "avc1.640028", "acodec": "none",
        "width": height * 16 / 9, "height": height, "fps": 30
    })
}

fn audio_format(format_id: &str, url: &str, language: &str, preference: i64) -> Value {
    json!({
        "format_id": format_id, "url": url, "protocol": "https", "ext": "m4a",
        "container": "m4a_dash", "vcodec": "none", "acodec": "mp4a.40.2", "abr": 128,
        "language": language, "language_preference": preference
    })
}

/// Снимок с двумя video-only строками, двумя языками и compound-выбором «720p + en».
fn snapshot_with_compound_default() -> YtDlpCandidateSnapshot {
    let video_720 = video_format("v720", "https://media.invalid/v720.mp4", 720);
    let audio_en = audio_format("a-en", "https://media.invalid/a-en.m4a", "en", 10);
    let document: YtDlpCandidateDocument = serde_json::from_value(json!({
        "format_id": "v720+a-en",
        "requested_formats": [video_720.clone(), audio_en.clone()],
        "formats": [
            video_format("v360", "https://media.invalid/v360.mp4", 360),
            video_720,
            audio_en,
            audio_format("a-ru", "https://media.invalid/a-ru.m4a", "ru", -1),
        ]
    }))
    .expect("synthetic yt-dlp document");
    normalize_candidate_document(
        document,
        SourceIdentity::new(91),
        ExtractionGeneration::new(1),
    )
}

fn compound_selection(snapshot: &YtDlpCandidateSnapshot) -> YtDlpCandidateSelection {
    let compound = snapshot
        .accepted_candidates()
        .find(|candidate| {
            matches!(
                candidate.descriptor().layout(),
                StreamLayout::Separate { .. }
            )
        })
        .expect("selected compound candidate is accepted");
    snapshot
        .selection_for(compound)
        .expect("compound selection")
}

#[test]
fn compound_default_resolves_to_its_own_video_only_row_and_composes_with_another_language() {
    let snapshot = snapshot_with_compound_default();
    let video = snapshot
        .inventory_video_of_compound(&compound_selection(&snapshot))
        .expect("the 720p row of the compound must be found by its request material");
    let russian = snapshot
        .accepted_candidates()
        .find(|candidate| {
            matches!(candidate.descriptor().layout(), StreamLayout::AudioOnly(audio)
                if audio.audio().language().is_some_and(|tag| tag.as_str() == "ru"))
        })
        .expect("russian audio row");
    let audio = snapshot.selection_for(russian).expect("audio selection");

    let composed = snapshot
        .compose_inventory_av(&video, &audio)
        .expect("the found video row composes with another language");

    let StreamLayout::Separate {
        video: component, ..
    } = composed.descriptor().layout()
    else {
        panic!("composition must be a separate pair");
    };
    // Именно 720p, а не соседняя строка 360p: видео при смене языка не меняется.
    assert_eq!(
        component.video().height().map(|height| height.pixels()),
        Some(720)
    );
}

#[test]
fn a_plain_inventory_row_is_not_a_compound() {
    let snapshot = snapshot_with_compound_default();
    let plain = snapshot
        .accepted_candidates()
        .find(|candidate| matches!(candidate.descriptor().layout(), StreamLayout::VideoOnly(_)))
        .expect("video-only row");
    let selection = snapshot.selection_for(plain).expect("selection");

    assert!(snapshot.inventory_video_of_compound(&selection).is_none());
}
