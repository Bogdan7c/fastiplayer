//! План восстановления звука: что выбрать по запомненному описанию и реальным дорожкам.

use media_core::{TrackId, TrackKind, TrackLabels, TrackLanguage};
use player_core::TrackSummarySnapshot;

use super::*;
use crate::state::track_memory::RememberedAudioPlan;
use crate::state::track_memory::hints::{audio_hint_for_track, plan_audio_restore};

fn row(id: u32, kind: TrackKind, codec: &str, language: &str, title: &str) -> TrackSummarySnapshot {
    TrackSummarySnapshot {
        id: TrackId::new(id),
        kind,
        codec_id: codec.to_owned(),
        sample_rate: None,
        channels: None,
        duration: None,
        video: None,
        video_color_summary: None,
        labels: TrackLabels {
            language: Some(TrackLanguage::new(language)),
            title: Some(title.to_owned()),
            disposition: Default::default(),
        },
    }
}

/// Видео + два звука + субтитры: ordinal считается только среди звука.
fn movie_tracks() -> Vec<TrackSummarySnapshot> {
    vec![
        row(10, TrackKind::Video, "V_VP8", "eng", "Main video"),
        row(11, TrackKind::Audio, "A_FLAC", "rus", "Dub"),
        row(12, TrackKind::Audio, "A_OPUS", "eng", "Original"),
        row(13, TrackKind::Subtitle, "S_TEXT/UTF8", "eng", "English"),
    ]
}

#[test]
fn a_remembered_second_audio_track_is_selected_over_the_default_first() {
    let tracks = movie_tracks();
    let hint = audio_hint_for_track(&tracks, TrackId::new(12))
        .expect("hint is valid")
        .expect("audio track exists");

    let plan = plan_audio_restore(Some(&hint), &tracks, Some(TrackId::new(11)));

    assert_eq!(plan, RememberedAudioPlan::Select(TrackId::new(12)));
}

#[test]
fn the_hint_counts_the_ordinal_among_audio_tracks_only() {
    let tracks = movie_tracks();

    let hint = audio_hint_for_track(&tracks, TrackId::new(12))
        .expect("hint")
        .expect("exists");

    assert_eq!(
        hint.ordinal_among_kind(),
        1,
        "second audio, not the third row"
    );
    assert_eq!(hint.language(), Some("eng"));
    assert_eq!(hint.title(), Some("Original"));
    assert_eq!(hint.codec_id(), "A_OPUS");
}

#[test]
fn only_audio_tracks_can_be_remembered_as_audio() {
    let tracks = movie_tracks();

    assert_eq!(
        audio_hint_for_track(&tracks, TrackId::new(10)).expect("ok"),
        None
    );
    assert_eq!(
        audio_hint_for_track(&tracks, TrackId::new(13)).expect("ok"),
        None
    );
    assert_eq!(
        audio_hint_for_track(&tracks, TrackId::new(99)).expect("ok"),
        None
    );
}

#[test]
fn the_same_track_chosen_by_default_needs_no_command() {
    let tracks = movie_tracks();
    let hint = audio_hint_for_track(&tracks, TrackId::new(11))
        .expect("hint")
        .expect("exists");

    let plan = plan_audio_restore(Some(&hint), &tracks, Some(TrackId::new(11)));

    assert_eq!(plan, RememberedAudioPlan::AlreadySelected);
}

#[test]
fn a_track_that_vanished_falls_back_to_the_default_without_error() {
    let tracks = movie_tracks();
    let hint = audio_hint("jpn", "Japanese", "A_AC3", 2);

    let plan = plan_audio_restore(Some(&hint), &tracks, Some(TrackId::new(11)));

    assert_eq!(plan, RememberedAudioPlan::TrackNotFound);
}

#[test]
fn nothing_remembered_means_nothing_to_do() {
    let plan = plan_audio_restore(None, &movie_tracks(), Some(TrackId::new(11)));

    assert_eq!(plan, RememberedAudioPlan::NothingRemembered);
}

#[test]
fn a_renumbered_container_still_finds_the_track_by_language_title_and_codec() {
    // Новая версия файла: ids другие, дорожки переставлены, но язык/название/кодек те же.
    let hint = audio_hint_for_track(&movie_tracks(), TrackId::new(12))
        .expect("hint")
        .expect("exists");
    let remuxed = vec![
        row(1, TrackKind::Video, "V_VP8", "eng", "Main video"),
        row(2, TrackKind::Audio, "A_OPUS", "eng", "Original"),
        row(3, TrackKind::Audio, "A_FLAC", "rus", "Dub"),
    ];

    let plan = plan_audio_restore(Some(&hint), &remuxed, Some(TrackId::new(3)));

    assert_eq!(plan, RememberedAudioPlan::Select(TrackId::new(2)));
}
