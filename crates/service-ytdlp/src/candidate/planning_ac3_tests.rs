//! AC-3/E-AC-3 строки yt-dlp: нормализация → planning requirement → audio fallback rank.

use audio_core::AudioDecodeCodecFamily;
use serde_json::{Value, json};
use web_media_core::{CodecFamily, CodecKind, ExtractionGeneration, SourceIdentity, StreamLayout};
use web_media_playback_plan::CandidateRuntimeRequirements;

use super::YtDlpCandidateSnapshot;
use super::raw::YtDlpCandidateDocument;

/// Audio-only progressive row в форме, которую yt-dlp отдаёт для m4a/mp4 audio.
fn audio_only_row(format_id: &str, audio_codec: &str, channels: u16) -> Value {
    json!({
        "format_id": format_id,
        "url": format!("https://media.invalid/{format_id}"),
        "protocol": "https",
        "ext": "m4a",
        "container": "m4a_dash",
        "vcodec": "none",
        "acodec": audio_codec,
        "audio_channels": channels,
        "asr": 48_000,
        "abr": 256
    })
}

/// Нормализует synthetic yt-dlp document production-путём.
fn snapshot(formats: Vec<Value>) -> YtDlpCandidateSnapshot {
    let document: YtDlpCandidateDocument = serde_json::from_value(json!({ "formats": formats }))
        .expect("synthetic yt-dlp document should be deserialized");
    super::normalize_candidate_document(
        document,
        SourceIdentity::new(91),
        ExtractionGeneration::new(3),
    )
}

/// `ac-3` и `eac3` проходят planning с точной family и сохраняют число каналов.
#[test]
fn dolby_audio_rows_plan_with_exact_family_and_channel_count() {
    let snapshot = snapshot(vec![
        audio_only_row("dolby-ac3", "ac-3", 6),
        audio_only_row("dolby-eac3", "eac3", 2),
    ]);
    let projection = snapshot
        .planning_projection()
        .expect("Dolby audio rows must form planning projection");
    assert!(
        projection.rejections().is_empty(),
        "AC-3/E-AC-3 rows must not be rejected: {:?}",
        projection.rejections()
    );

    let mut observed = projection
        .snapshot()
        .candidates()
        .iter()
        .map(|candidate| {
            let StreamLayout::AudioOnly(component) = candidate.descriptor().layout() else {
                panic!("audio-only row must stay audio-only");
            };
            let CandidateRuntimeRequirements::AudioOnly { audio } =
                candidate.runtime_requirements()
            else {
                panic!("audio-only row must carry audio-only requirement");
            };
            (
                component.audio().codec().kind(),
                *audio,
                component
                    .audio()
                    .channels()
                    .map(web_media_core::ChannelCount::get),
            )
        })
        .collect::<Vec<_>>();
    observed.sort_by_key(|(_, family, _)| *family as u8);

    assert_eq!(
        observed,
        vec![
            (
                CodecKind::Known(CodecFamily::Ac3),
                AudioDecodeCodecFamily::Ac3,
                Some(6)
            ),
            (
                CodecKind::Known(CodecFamily::Eac3),
                AudioDecodeCodecFamily::Eac3,
                Some(2)
            ),
        ]
    );
}

/// Прежняя политика сохраняется: stereo AAC остаётся лучше 5.1 E-AC-3, а между
/// двумя Dolby rows действует обычная yt-dlp семантика (каналы, затем codec rank).
#[test]
fn dolby_rows_are_fallback_only_in_audio_rank() {
    let snapshot = snapshot(vec![
        audio_only_row("surround-eac3", "ec-3", 6),
        audio_only_row("stereo-aac", "mp4a.40.2", 2),
        audio_only_row("stereo-eac3", "ec-3", 2),
        audio_only_row("stereo-ac3", "ac-3", 2),
    ]);
    let rank_of = |format_id: &str| {
        snapshot
            .accepted_candidates()
            .find(|candidate| candidate.descriptor().identity().format().as_str() == format_id)
            .unwrap_or_else(|| panic!("row {format_id} must be accepted"))
            .audio_fallback_rank()
            .expect("audio-only row has audio rank")
    };

    assert!(rank_of("stereo-aac") > rank_of("surround-eac3"));
    assert!(rank_of("surround-eac3") > rank_of("stereo-eac3"));
    assert!(rank_of("stereo-eac3") > rank_of("stereo-ac3"));
}
