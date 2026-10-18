//! Единый список меню «♪»: web-версии звука (HLS/DASH renditions) + встроенные дорожки,
//! клавиша `B` по всему списку и уведомления о старте/отказе переключения версии.

use std::sync::Arc;

use media_core::{TrackDisposition, TrackId, TrackKind, TrackLabels, TrackLanguage};
use player_core::{PlayerSnapshot, TrackSummarySnapshot};
use web_media_core::{CodecFamily, ComponentVariantCatalogGeneration, DynamicRange};

use super::control_actions::{NextTrackPlan, plan_next_track};
use super::menu_view::{AudioTrackMenuTarget, AudioTrackMenuView, WebAudioMenuSource};
use super::web_version_switch::{
    VERSION_SWITCH_BUSY_MESSAGE, VERSION_SWITCH_CANCELED_MESSAGE, VERSION_SWITCH_FAILED_MESSAGE,
    VERSION_SWITCH_STALE_MESSAGE, start_notice, terminal_failure_notice,
};
use super::web_versions::{channel_layout_label, web_audio_versions};
use crate::state::same_item_candidate_switch::{SameItemSwitchError, UrlSidebarActionApplyOutcome};
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionError, ComponentVariantSelectionAction,
    WebMediaAudioComponentVariantAxis, WebMediaAudioComponentVariantPresentation,
    WebMediaComponentVariantAxisKind, WebMediaComponentVariantProjection,
    WebMediaCoupledComponentVariantAxis, WebMediaInstalledComponentVariantPresentation,
    WebMediaVideoComponentVariantAxis, WebMediaVideoComponentVariantPresentation,
};
use crate::web_media_stream_model::{UrlSidebarSafeError, WebMediaStreamGeneration};

/// Generation-fence-ы фикстуры: action обязан нести ровно их.
const CATALOG_GENERATION: u64 = 21;

fn parent_generation() -> WebMediaStreamGeneration {
    WebMediaStreamGeneration::for_test(5, 3)
}

fn audio_row(
    language: Option<&str>,
    codec: CodecFamily,
    channels: u16,
    bitrate: u64,
) -> WebMediaAudioComponentVariantPresentation {
    WebMediaAudioComponentVariantPresentation {
        language_label: language.map(Arc::from),
        bitrate: Some(bitrate),
        sample_rate_hz: Some(48_000),
        channels: Some(channels),
        codec: Some(codec),
    }
}

fn video_axis() -> WebMediaVideoComponentVariantAxis {
    WebMediaVideoComponentVariantAxis {
        active_index: 0,
        variants: Arc::from(vec![WebMediaVideoComponentVariantPresentation {
            width: Some(1_280),
            height: Some(720),
            frame_rate: Some((30, 1)),
            bitrate: Some(2_000_000),
            codec: Some(CodecFamily::H264),
            dynamic_range: DynamicRange::Sdr,
        }]),
    }
}

/// HLS master: видео + audio group из AAC 2.0 и E-AC-3 5.1 (как у Dolby-раздач).
fn aac_and_eac3_projection(active_index: usize) -> WebMediaComponentVariantProjection {
    WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoAndAudio {
            catalog_generation: ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
            video: video_axis(),
            audio: WebMediaAudioComponentVariantAxis {
                active_index,
                variants: Arc::from(vec![
                    audio_row(Some("en"), CodecFamily::Aac, 2, 128_000),
                    audio_row(Some("en"), CodecFamily::Eac3, 6, 384_000),
                ]),
            },
        },
    )
}

fn audio_action(variant_index: usize) -> ComponentVariantSelectionAction {
    ComponentVariantSelectionAction {
        parent_generation: parent_generation(),
        catalog_generation: ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
        axis: WebMediaComponentVariantAxisKind::Audio,
        variant_index,
    }
}

fn audio_track(raw_id: u32, codec_id: &str, language: &str) -> TrackSummarySnapshot {
    TrackSummarySnapshot {
        id: TrackId::new(raw_id),
        kind: TrackKind::Audio,
        codec_id: codec_id.to_owned(),
        sample_rate: None,
        channels: None,
        duration: None,
        video: None,
        video_color_summary: None,
        labels: TrackLabels {
            language: Some(TrackLanguage::new(language)),
            title: None,
            disposition: TrackDisposition::default(),
        },
    }
}

/// Snapshot web-источника: одна встроенная дорожка — звук установленной версии.
fn single_embedded_snapshot() -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.tracks = vec![audio_track(1, "A_AAC", "eng")];
    snapshot.selected_tracks.audio_track = Some(TrackId::new(1));
    snapshot
}

fn menu_for(
    projection: &WebMediaComponentVariantProjection,
    snapshot: &PlayerSnapshot,
) -> AudioTrackMenuView {
    AudioTrackMenuView::compose(
        snapshot,
        web_audio_versions(parent_generation(), projection).map(WebAudioMenuSource::Versions),
    )
}

fn rows(view: &AudioTrackMenuView) -> Vec<(AudioTrackMenuTarget, &str, bool)> {
    view.tracks()
        .iter()
        .map(|item| (item.target, item.label.as_str(), item.is_selected))
        .collect()
}

#[test]
fn hls_master_with_aac_and_eac3_lists_both_labeled_and_marks_active() {
    let view = menu_for(&aac_and_eac3_projection(0), &single_embedded_snapshot());

    assert!(view.has_choice(), "two versions make the button visible");
    assert_eq!(
        rows(&view),
        vec![
            (
                AudioTrackMenuTarget::WebAudioVersion(audio_action(0)),
                "English (AAC 2.0)",
                true
            ),
            (
                AudioTrackMenuTarget::WebAudioVersion(audio_action(1)),
                "English (E-AC-3 5.1)",
                false
            ),
        ],
        "the single embedded track duplicates the active version and is not listed"
    );
}

#[test]
fn active_mark_follows_the_installed_version() {
    let view = menu_for(&aac_and_eac3_projection(1), &single_embedded_snapshot());
    let selected: Vec<bool> = view.tracks().iter().map(|item| item.is_selected).collect();
    assert_eq!(selected, vec![false, true]);
}

#[test]
fn b_cycles_from_aac_to_eac3_and_wraps_back() {
    let from_aac = plan_next_track(&menu_for(
        &aac_and_eac3_projection(0),
        &single_embedded_snapshot(),
    ));
    assert_eq!(
        from_aac,
        NextTrackPlan::Switch {
            target: AudioTrackMenuTarget::WebAudioVersion(audio_action(1)),
            label: "English (E-AC-3 5.1)".to_owned(),
        }
    );
    let from_eac3 = plan_next_track(&menu_for(
        &aac_and_eac3_projection(1),
        &single_embedded_snapshot(),
    ));
    assert_eq!(
        from_eac3,
        NextTrackPlan::Switch {
            target: AudioTrackMenuTarget::WebAudioVersion(audio_action(0)),
            label: "English (AAC 2.0)".to_owned(),
        }
    );
}

#[test]
fn web_versions_replace_embedded_tracks_so_only_one_entry_is_marked() {
    // Два встроенных трека у web-источника: меню всё равно показывает версии (одна отметка).
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.tracks = vec![
        audio_track(1, "A_AAC", "eng"),
        audio_track(2, "A_AAC", "rus"),
    ];
    snapshot.selected_tracks.audio_track = Some(TrackId::new(2));
    let view = menu_for(&aac_and_eac3_projection(0), &snapshot);

    let targets: Vec<AudioTrackMenuTarget> = view.tracks().iter().map(|item| item.target).collect();
    assert_eq!(
        targets,
        vec![
            AudioTrackMenuTarget::WebAudioVersion(audio_action(0)),
            AudioTrackMenuTarget::WebAudioVersion(audio_action(1)),
        ]
    );
    assert_eq!(
        view.tracks().iter().filter(|item| item.is_selected).count(),
        1
    );
}

#[test]
fn b_visits_every_web_version_of_a_three_version_master() {
    let projection_with_active = |active_index| {
        WebMediaComponentVariantProjection::Installed(
            WebMediaInstalledComponentVariantPresentation::VideoAndAudio {
                catalog_generation: ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
                video: video_axis(),
                audio: WebMediaAudioComponentVariantAxis {
                    active_index,
                    variants: Arc::from(vec![
                        audio_row(Some("en"), CodecFamily::Aac, 2, 128_000),
                        audio_row(Some("en"), CodecFamily::Ac3, 6, 384_000),
                        audio_row(Some("en"), CodecFamily::Eac3, 6, 640_000),
                    ]),
                },
            },
        )
    };
    let mut active_index = 0;
    let mut visited = Vec::new();
    for _ in 0..3 {
        let view = menu_for(
            &projection_with_active(active_index),
            &single_embedded_snapshot(),
        );
        let NextTrackPlan::Switch {
            target: AudioTrackMenuTarget::WebAudioVersion(action),
            ..
        } = plan_next_track(&view)
        else {
            panic!("B must switch to the next web version");
        };
        visited.push(action.variant_index);
        // Как после успешного switch-а: следующий кадр видит новую активную версию.
        active_index = action.variant_index;
    }
    assert_eq!(visited, vec![1, 2, 0]);
}

#[test]
fn media_without_web_versions_keeps_the_embedded_only_menu() {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.tracks = vec![
        audio_track(2, "A_FLAC", "rus"),
        audio_track(3, "A_OPUS", "eng"),
    ];
    snapshot.selected_tracks.audio_track = Some(TrackId::new(2));
    let embedded_only = AudioTrackMenuView::from_snapshot(&snapshot);

    let coupled = WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled {
            catalog_generation: ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
            coupled: WebMediaCoupledComponentVariantAxis {
                active_index: 0,
                variants: Arc::from(Vec::new()),
            },
        },
    );
    let single_version = WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::AudioOnly {
            catalog_generation: ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
            audio: WebMediaAudioComponentVariantAxis {
                active_index: 0,
                variants: Arc::from(vec![audio_row(Some("en"), CodecFamily::Aac, 2, 1)]),
            },
        },
    );
    let video_only = WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoOnly {
            catalog_generation: ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
            video: video_axis(),
        },
    );
    for projection in [
        WebMediaComponentVariantProjection::Unavailable,
        coupled,
        single_version,
        video_only,
    ] {
        assert_eq!(web_audio_versions(parent_generation(), &projection), None);
        assert_eq!(menu_for(&projection, &snapshot), embedded_only);
    }
    assert_eq!(
        rows(&embedded_only),
        vec![
            (
                AudioTrackMenuTarget::EmbeddedTrack(TrackId::new(2)),
                "Russian (FLAC)",
                true
            ),
            (
                AudioTrackMenuTarget::EmbeddedTrack(TrackId::new(3)),
                "English (Opus)",
                false
            ),
        ]
    );
}

#[test]
fn version_labels_fall_back_and_disambiguate_with_bitrate() {
    let projection = WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::AudioOnly {
            catalog_generation: ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
            audio: WebMediaAudioComponentVariantAxis {
                active_index: 0,
                variants: Arc::from(vec![
                    audio_row(Some("en"), CodecFamily::Aac, 2, 64_000),
                    audio_row(Some("en"), CodecFamily::Aac, 2, 128_000),
                    audio_row(None, CodecFamily::Ac3, 6, 448_000),
                    WebMediaAudioComponentVariantPresentation {
                        language_label: None,
                        bitrate: None,
                        sample_rate_hz: None,
                        channels: None,
                        codec: None,
                    },
                    audio_row(Some("de"), CodecFamily::Opus, 8, 256_000),
                ]),
            },
        },
    );
    let Some(versions) = web_audio_versions(parent_generation(), &projection) else {
        panic!("five versions form a choice");
    };
    let labels: Vec<&str> = versions
        .iter()
        .map(|version| version.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec![
            "English (AAC 2.0, 64 kbps)",
            "English (AAC 2.0, 128 kbps)",
            "Audio version 3 (AC-3 5.1)",
            "Audio version 4",
            "German (Opus 7.1)",
        ]
    );
}

#[test]
fn channel_layout_labels_use_common_names_and_honest_fallback() {
    assert_eq!(channel_layout_label(1), "Mono");
    assert_eq!(channel_layout_label(2), "2.0");
    assert_eq!(channel_layout_label(6), "5.1");
    assert_eq!(channel_layout_label(8), "7.1");
    assert_eq!(channel_layout_label(3), "3 ch");
}

#[test]
fn start_notice_names_the_version_or_the_failure_reason() {
    assert_eq!(
        start_notice(
            &Ok(UrlSidebarActionApplyOutcome::Started),
            Some("English (E-AC-3 5.1)")
        ),
        Some("Audio: English (E-AC-3 5.1)".to_owned())
    );
    assert_eq!(
        start_notice(&Ok(UrlSidebarActionApplyOutcome::NoChange), Some("x")),
        None,
        "re-selecting the active version stays silent"
    );
    assert_eq!(
        start_notice(&Err(SameItemSwitchError::Busy), None),
        Some(VERSION_SWITCH_BUSY_MESSAGE.to_owned())
    );
    assert_eq!(
        start_notice(
            &Err(SameItemSwitchError::ComponentAction(
                ComponentVariantActionError::Unavailable
            )),
            None
        ),
        Some(VERSION_SWITCH_STALE_MESSAGE.to_owned())
    );
    assert_eq!(
        start_notice(
            &Err(SameItemSwitchError::ComponentAction(
                ComponentVariantActionError::ReplacementFailed(
                    web_media_core::ComponentVariantError::IncompatibleComponentPair
                )
            )),
            None
        ),
        Some(VERSION_SWITCH_FAILED_MESSAGE.to_owned()),
        "a version that cannot be paired with the current picture is a failure, not stale"
    );
    assert_eq!(
        start_notice(&Err(SameItemSwitchError::UnsupportedSource), None),
        Some(VERSION_SWITCH_FAILED_MESSAGE.to_owned())
    );
}

#[test]
fn terminal_failure_notice_maps_every_safe_category() {
    assert_eq!(
        terminal_failure_notice(UrlSidebarSafeError::SourceUnavailable),
        VERSION_SWITCH_FAILED_MESSAGE
    );
    assert_eq!(
        terminal_failure_notice(UrlSidebarSafeError::SameItemSwitchBusy),
        VERSION_SWITCH_BUSY_MESSAGE
    );
    assert_eq!(
        terminal_failure_notice(UrlSidebarSafeError::SameItemSwitchStale),
        VERSION_SWITCH_STALE_MESSAGE
    );
    assert_eq!(
        terminal_failure_notice(UrlSidebarSafeError::SameItemSwitchCancelled),
        VERSION_SWITCH_CANCELED_MESSAGE
    );
}
