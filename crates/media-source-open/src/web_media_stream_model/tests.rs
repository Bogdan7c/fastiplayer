//! Доменные тесты web-media stream model: generation fence, preference и
//! сохранение installed HLS subtitle descriptors. UI-проекция URL sidebar
//! тестируется в `app-egui` (`web_media_stream_model/tests.rs`).

use super::*;

pub(super) fn candidate(height: Option<u32>, audio_only: bool) -> WebMediaCandidatePresentation {
    WebMediaCandidatePresentation {
        layout: if audio_only {
            StreamLayoutKind::AudioOnly
        } else {
            StreamLayoutKind::Muxed
        },
        width: height.map(|value| value * 16 / 9),
        height,
        frame_rate: height.map(|_| (30, 1)),
        video_bitrate: height.map(|_| 4_000_000),
        audio_bitrate: Some(128_000),
        video_codec: (!audio_only).then_some(CodecFamily::Vp9),
        audio_codec: Some(CodecFamily::Opus),
        dynamic_range: (!audio_only).then_some(web_media_core::DynamicRange::Sdr),
        containers: WebMediaContainerSummary {
            video: (!audio_only).then_some(ContainerFamily::WebM),
            audio: Some(ContainerFamily::WebM),
        },
    }
}

fn configuration(
    active_parent: ExactSelectionIdentity,
    candidates: Vec<WebMediaCandidatePresentation>,
    active_candidate: WebMediaCandidatePresentation,
) -> WebMediaStreamConfiguration {
    WebMediaStreamConfiguration::fixture(
        active_parent,
        candidates,
        active_candidate,
        WebMediaSelectionPreference::GlobalBestPlayable,
    )
}

fn exact_parent(source: u64, extraction: u64) -> ExactSelectionIdentity {
    let source = web_media_core::SourceIdentity::new(source);
    let exact = web_media_core::CandidateIdentity::new(
        source,
        web_media_core::ExtractionGeneration::new(extraction),
        web_media_core::CandidateFormatIdentity::new("active-parent")
            .expect("fixture exact identity валидна"),
    );
    let semantic = web_media_core::SemanticIdentity::new(source, "semantic-parent")
        .expect("fixture semantic identity валидна");
    ExactSelectionIdentity::new(exact, semantic).expect("fixture source lineage совпадает")
}

#[test]
fn installed_hls_subtitles_survive_configuration_clone_without_locator() {
    let parent = exact_parent(1, 1);
    let active_candidate = candidate(Some(720), false);
    let rendition = crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition::fixture(
        "subs",
        "English",
        Some("en"),
        Some("public.accessibility.transcribes-spoken-dialog"),
        false,
    );
    let configured = configuration(parent, vec![active_candidate.clone()], active_candidate)
        .with_hls_subtitle_renditions(Arc::from([rendition]));
    let rebuilt = configured.clone();
    let [retained] = rebuilt.hls_subtitle_renditions() else {
        panic!("exact installed rendition должен сохраниться");
    };
    assert_eq!(retained.group_id(), "subs");
    assert_eq!(retained.name(), "English");
    assert_eq!(retained.language(), Some("en"));
    assert!(!format!("{retained:?}").contains("://"));
}

#[test]
fn stale_generation_cannot_resolve_neutral_switch_selection() {
    let current_generation = WebMediaStreamGeneration::for_test(31, 7);
    let stale_generation = WebMediaStreamGeneration::for_test(31, 6);
    let active_candidate = candidate(Some(720), false);
    let configuration = configuration(
        exact_parent(31, 7),
        vec![active_candidate.clone()],
        active_candidate,
    );

    assert!(
        configuration
            .selection_for_switch(stale_generation, 0)
            .is_none(),
        "stale generation не должна получить exact neutral selection"
    );
    assert!(
        configuration
            .selection_for_switch(current_generation, 0)
            .is_some(),
        "matching generation должна получить bounded selection"
    );
}

#[test]
fn preference_distinguishes_global_default_and_item_override() {
    assert_ne!(
        WebMediaSelectionPreference::GlobalBestPlayable,
        WebMediaSelectionPreference::ItemOverride(None)
    );
    assert_ne!(
        WebMediaSelectionPreference::GlobalPreferredHeight(2160),
        WebMediaSelectionPreference::ItemOverride(Some(2160))
    );
}
