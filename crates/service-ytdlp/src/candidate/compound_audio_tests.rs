//! Audio-only строка inventory, из которой сделан compound yt-dlp, находится по request material.
//!
//! Форма снимка повторяет реальный YouTube `616+251`: yt-dlp выбрал HLS video-only
//! вместе с HTTPS audio-only, а рядом лежат progressive video-only альтернативы.

use serde_json::{Value, json};
use web_media_core::{ExtractionGeneration, SourceIdentity, StreamLayout, TransportFamily};

use super::model::{YtDlpCandidateSelection, YtDlpCandidateSnapshot};
use super::normalize::normalize_candidate_document;
use super::raw::YtDlpCandidateDocument;

/// HLS video-only строка (`m3u8_native`), как YouTube «Premium» format 616.
fn hls_video_format() -> Value {
    json!({
        "format_id": "616", "url": "https://media.invalid/616/index.m3u8",
        "manifest_url": "https://media.invalid/master.m3u8", "protocol": "m3u8_native",
        "ext": "mp4", "vcodec": "vp09.00.40.08", "acodec": "none",
        "width": 1920, "height": 1080, "fps": 25, "tbr": 4700
    })
}

/// Progressive HTTPS video-only строка.
fn progressive_video_format() -> Value {
    json!({
        "format_id": "248", "url": "https://media.invalid/248.webm", "protocol": "https",
        "ext": "webm", "container": "webm_dash", "vcodec": "vp9", "acodec": "none",
        "width": 1920, "height": 1080, "fps": 25, "tbr": 1800
    })
}

/// HTTPS audio-only строка заданного языка.
fn audio_format(format_id: &str, language: &str, preference: i64) -> Value {
    json!({
        "format_id": format_id, "url": format!("https://media.invalid/{format_id}.webm"),
        "protocol": "https", "ext": "webm", "container": "webm_dash", "vcodec": "none",
        "acodec": "opus", "abr": 125, "language": language, "language_preference": preference
    })
}

/// Снимок с compound-выбором «HLS 616 + HTTPS 251 (en)» и dub-дорожкой рядом.
fn mixed_transport_compound_snapshot() -> YtDlpCandidateSnapshot {
    let video = hls_video_format();
    let audio_en = audio_format("251-en", "en", 10);
    let document: YtDlpCandidateDocument = serde_json::from_value(json!({
        "format_id": "616+251-en",
        "requested_formats": [video.clone(), audio_en.clone()],
        "formats": [
            progressive_video_format(),
            video,
            audio_format("251-de", "de", -1),
            audio_en,
        ]
    }))
    .expect("synthetic yt-dlp document");
    normalize_candidate_document(
        document,
        SourceIdentity::new(616),
        ExtractionGeneration::new(1),
    )
}

/// Selection accepted compound-а (единственный Separate кандидат снимка).
fn compound_selection(snapshot: &YtDlpCandidateSnapshot) -> YtDlpCandidateSelection {
    let compound = snapshot
        .accepted_candidates()
        .find(|candidate| {
            matches!(
                candidate.descriptor().layout(),
                StreamLayout::Separate { .. }
            )
        })
        .expect("mixed-transport compound is accepted by normalization");
    snapshot
        .selection_for(compound)
        .expect("compound selection")
}

#[test]
fn mixed_transport_compound_resolves_its_own_audio_row_not_a_dub() {
    let snapshot = mixed_transport_compound_snapshot();

    let audio = snapshot
        .inventory_audio_of_compound(&compound_selection(&snapshot))
        .expect("compound audio row must be found by its request material");

    let StreamLayout::AudioOnly(component) = audio.descriptor().layout() else {
        panic!("compound audio must resolve to an audio-only inventory row");
    };
    // Именно язык, выбранный yt-dlp (en), а не соседний dub (de).
    assert_eq!(
        component.audio().language().map(|tag| tag.as_str()),
        Some("en")
    );
    assert!(matches!(
        component.transport().family(),
        TransportFamily::ProgressiveHttp(_)
    ));

    // Найденная audio-строка композируется с progressive video-only альтернативой.
    let progressive_video = snapshot
        .accepted_candidates()
        .find(|candidate| {
            matches!(candidate.descriptor().layout(), StreamLayout::VideoOnly(video)
                if matches!(video.transport().family(), TransportFamily::ProgressiveHttp(_)))
        })
        .expect("progressive video-only row");
    let video = snapshot
        .selection_for(progressive_video)
        .expect("video selection");
    let composed = snapshot
        .compose_inventory_av(&video, &audio)
        .expect("progressive video composes with compound audio");
    let StreamLayout::Separate {
        video: composed_video,
        audio: composed_audio,
    } = composed.descriptor().layout()
    else {
        panic!("composition must be a separate pair");
    };
    assert!(
        composed_video
            .transport()
            .family()
            .shares_component_composition_route(composed_audio.transport().family())
    );
}

#[test]
fn plain_audio_row_is_not_a_compound() {
    let snapshot = mixed_transport_compound_snapshot();
    let plain_audio = snapshot
        .accepted_candidates()
        .find(|candidate| matches!(candidate.descriptor().layout(), StreamLayout::AudioOnly(_)))
        .expect("audio-only row");
    let selection = snapshot.selection_for(plain_audio).expect("selection");

    assert!(snapshot.inventory_audio_of_compound(&selection).is_none());
}
