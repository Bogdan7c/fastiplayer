//! Меню «♪» с языками звука yt-dlp-видео: подписи, отметка «играет», круг клавиши `B`.

use media_core::{TrackKind, TrackLabels};
use media_source_open::web_media_open::audio_languages::{
    WebAudioLanguageAction, WebAudioLanguageChoice,
};
use player_core::{PlayerSnapshot, TrackSummarySnapshot};
use web_media_core::LanguageTag;

use super::control_actions::{NextTrackPlan, plan_next_track};
use super::menu_view::{AudioTrackMenuTarget, AudioTrackMenuView, WebAudioMenuSource};
use super::web_audio_languages::web_audio_languages;
use crate::web_media_stream_model::WebMediaStreamGeneration;

fn action(route_index: usize) -> WebAudioLanguageAction {
    WebAudioLanguageAction::for_test(WebMediaStreamGeneration::for_test(7, 1), route_index)
}

fn choice(
    route_index: usize,
    tag: &str,
    is_original: bool,
    is_active: bool,
) -> WebAudioLanguageChoice {
    WebAudioLanguageChoice {
        action: action(route_index),
        language: LanguageTag::new(tag).expect("language tag"),
        is_original,
        is_active,
    }
}

/// Оригинал `en-US` играет, дубляжи `ru` и `de` доступны.
fn three_choices() -> Vec<WebAudioLanguageChoice> {
    vec![
        choice(0, "en-US", true, true),
        choice(1, "ru", false, false),
        choice(2, "de", false, false),
    ]
}

/// Snapshot с одной встроенной дорожкой: язык-меню не должен её показывать.
fn snapshot_with_embedded_track() -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.tracks = vec![TrackSummarySnapshot {
        id: media_core::TrackId::new(1),
        kind: TrackKind::Audio,
        codec_id: "A_OPUS".to_owned(),
        sample_rate: None,
        channels: None,
        duration: None,
        video: None,
        video_color_summary: None,
        labels: TrackLabels::default(),
    }];
    snapshot
}

fn languages_view(choices: Vec<WebAudioLanguageChoice>) -> AudioTrackMenuView {
    AudioTrackMenuView::compose(
        &snapshot_with_embedded_track(),
        web_audio_languages(choices).map(WebAudioMenuSource::Languages),
    )
}

fn rows(view: &AudioTrackMenuView) -> Vec<(AudioTrackMenuTarget, &str, bool)> {
    view.tracks()
        .iter()
        .map(|item| (item.target, item.label.as_str(), item.is_selected))
        .collect()
}

#[test]
fn menu_lists_languages_with_original_marked_and_active_selected() {
    let view = languages_view(three_choices());

    assert_eq!(
        rows(&view),
        [
            (
                AudioTrackMenuTarget::WebAudioLanguage(action(0)),
                "English (original)",
                true
            ),
            (
                AudioTrackMenuTarget::WebAudioLanguage(action(2)),
                "German",
                false
            ),
            (
                AudioTrackMenuTarget::WebAudioLanguage(action(1)),
                "Russian",
                false
            ),
        ],
        "embedded tracks are hidden and exactly one row is marked as playing"
    );
    assert!(view.has_choice());
}

#[test]
fn a_dub_marked_active_moves_the_selection_mark() {
    let view = languages_view(vec![
        choice(0, "en-US", true, false),
        choice(1, "ru", false, true),
    ]);

    assert_eq!(
        view.tracks()
            .iter()
            .map(|item| item.is_selected)
            .collect::<Vec<_>>(),
        [false, true]
    );
}

#[test]
fn fewer_than_two_languages_fall_back_to_embedded_tracks() {
    let view = languages_view(vec![choice(0, "en-US", true, true)]);

    assert_eq!(
        view.tracks()
            .iter()
            .map(|item| item.target)
            .collect::<Vec<_>>(),
        [AudioTrackMenuTarget::EmbeddedTrack(
            media_core::TrackId::new(1)
        )]
    );
}

#[test]
fn a_tag_without_a_name_gets_a_numbered_fallback_label() {
    let entries = web_audio_languages(vec![
        choice(0, "und", false, true),
        choice(1, "ru", false, false),
    ])
    .expect("two languages");

    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.label.as_str())
            .collect::<Vec<_>>(),
        ["Audio language 1", "Russian"]
    );
}

#[test]
fn b_cycles_through_languages_and_wraps_to_the_original() {
    let after_original = plan_next_track(&languages_view(three_choices()));
    assert_eq!(
        after_original,
        NextTrackPlan::Switch {
            target: AudioTrackMenuTarget::WebAudioLanguage(action(2)),
            label: "German".to_owned(),
        }
    );

    let last_active = languages_view(vec![
        choice(0, "en-US", true, false),
        choice(1, "ru", false, true),
        choice(2, "de", false, false),
    ]);
    assert_eq!(
        plan_next_track(&last_active),
        NextTrackPlan::Switch {
            target: AudioTrackMenuTarget::WebAudioLanguage(action(0)),
            label: "English (original)".to_owned(),
        }
    );
}

#[test]
fn regional_dubs_of_one_language_get_distinct_labels_and_the_original_marker() {
    let entries = web_audio_languages(vec![
        choice(0, "pt-BR", true, true),
        choice(1, "pt-PT", false, false),
        choice(2, "en-US", false, false),
        choice(3, "ru", false, false),
    ])
    .expect("four languages are selectable");

    let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();

    assert_eq!(
        labels,
        [
            "Portuguese (pt-BR) (original)",
            "English",
            "Portuguese (pt-PT)",
            "Russian"
        ]
    );
}

#[test]
fn indian_and_legacy_codes_show_names_and_rows_are_sorted_after_the_original() {
    let entries = web_audio_languages(vec![
        choice(0, "ta", false, false),
        choice(1, "en-US", true, true),
        choice(2, "te", false, false),
        choice(3, "pa", false, false),
        choice(4, "ml", false, false),
        choice(5, "iw", false, false),
        choice(6, "de", false, false),
    ])
    .expect("seven languages are selectable");

    let shown: Vec<(usize, &str)> = entries
        .iter()
        .map(|entry| {
            let position = (0..7)
                .find(|index| action(*index) == entry.action)
                .expect("known action");
            (position, entry.label.as_str())
        })
        .collect();

    assert_eq!(
        shown,
        [
            (1, "English (original)"),
            (6, "German"),
            (5, "Hebrew"),
            (4, "Malayalam"),
            (3, "Punjabi"),
            (0, "Tamil"),
            (2, "Telugu"),
        ],
        "original first, then alphabetical by displayed name; raw codes never leak"
    );
}

#[test]
fn unnamed_tags_sort_alphabetically_after_the_original() {
    let entries = web_audio_languages(vec![
        choice(0, "xx-AA", false, false),
        choice(1, "en", true, false),
        choice(2, "xx-BB", false, false),
    ])
    .expect("three languages are selectable");
    let order: Vec<WebAudioLanguageAction> = entries.iter().map(|entry| entry.action).collect();
    assert_eq!(order, [action(1), action(0), action(2)]);
}
