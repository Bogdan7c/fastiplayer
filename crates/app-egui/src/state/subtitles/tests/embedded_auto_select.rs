//! Автовыбор с встроенными дорожками: язык, файл рядом важнее встроенной, флаг default,
//! повтор выбора при появлении встроенных дорожек.

use media_core::{TrackId, TrackLabels, TrackLanguage};

use super::embedded_support::*;
use super::track_selection::movie_with;
use super::*;

/// Строка снимка субтитровой дорожки SRT с языком и флагом default.
fn srt_row(raw_id: u32, language: &str, is_default: bool) -> player_core::TrackSummarySnapshot {
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let mut row = fixture.subtitle_row(0).clone();
    row.id = TrackId::new(raw_id);
    row.labels = TrackLabels {
        language: Some(TrackLanguage::new(language)),
        title: None,
        disposition: media_core::TrackDisposition {
            is_default,
            ..media_core::TrackDisposition::default()
        },
    };
    row
}

fn config_for(languages: &str) -> SubtitlesConfig {
    SubtitlesConfig {
        enabled: true,
        preferred_languages: languages.to_owned(),
        ..SubtitlesConfig::default()
    }
}

/// Какая строка меню отмечена: подпись происхождения и язык.
fn selected_label(owner: &SubtitleOwner) -> Option<(SubtitleFileOrigin, String)> {
    owner
        .menu_view()
        .tracks
        .into_iter()
        .find(|track| track.is_selected)
        .map(|track| {
            (
                track.label.origin,
                track
                    .label
                    .language
                    .map(|l| l.as_str().to_owned())
                    .unwrap_or_default(),
            )
        })
}

use super::super::menu_view::SubtitleFileOrigin;

#[test]
fn sidecar_file_beats_an_embedded_track_of_the_same_language() {
    let (_dir, video) = movie_with(&[("Movie.en.srt", "1\n00:00:01,000 --> 00:00:02,000\nSide\n")]);
    let media = instance(2);
    let mut owner = owner_with_config(&config_for("en"));
    begin_and_wait(&mut owner, media, &video);

    owner.sync_embedded_tracks(Some(media), &[srt_row(5, "eng", true)]);

    assert_eq!(
        selected_label(&owner),
        Some((SubtitleFileOrigin::NextToVideo, "en".to_owned())),
        "the sidecar file wins even though the embedded track is the container default"
    );
}

#[test]
fn preferred_language_beats_origin_and_default_flag() {
    let (_dir, video) = movie_with(&[("Movie.en.srt", "1\n00:00:01,000 --> 00:00:02,000\nSide\n")]);
    let media = instance(2);
    let mut owner = owner_with_config(&config_for("ru"));
    begin_and_wait(&mut owner, media, &video);

    owner.sync_embedded_tracks(
        Some(media),
        &[srt_row(5, "eng", true), srt_row(6, "rus", false)],
    );

    assert_eq!(
        selected_label(&owner),
        Some((SubtitleFileOrigin::Embedded, "rus".to_owned()))
    );
}

#[test]
fn among_embedded_tracks_of_one_language_the_container_default_wins() {
    let media = instance(2);
    let mut owner = owner_with_config(&config_for("en"));
    owner.begin_media(media, SubtitleMediaSource::NotLocal);

    owner.sync_embedded_tracks(
        Some(media),
        &[srt_row(5, "eng", false), srt_row(6, "eng", true)],
    );

    let selected = owner.menu_view().tracks.iter().position(|t| t.is_selected);
    assert_eq!(selected, Some(1), "the default-flagged track is chosen");
}

#[test]
fn without_a_language_match_sidecar_goes_first_then_embedded_default_then_first() {
    // Предпочтение «de», ни у кого нет: файл рядом.
    let (_dir, video) = movie_with(&[("Movie.fr.srt", "1\n00:00:01,000 --> 00:00:02,000\nSide\n")]);
    let media = instance(2);
    let mut owner = owner_with_config(&config_for("de"));
    begin_and_wait(&mut owner, media, &video);
    owner.sync_embedded_tracks(Some(media), &[srt_row(5, "eng", true)]);
    assert_eq!(
        selected_label(&owner),
        Some((SubtitleFileOrigin::NextToVideo, "fr".to_owned()))
    );

    // Только встроенные: дорожка с default, а не первая.
    let mut embedded_only = owner_with_config(&config_for("de"));
    embedded_only.begin_media(instance(3), SubtitleMediaSource::NotLocal);
    embedded_only.sync_embedded_tracks(
        Some(instance(3)),
        &[srt_row(5, "eng", false), srt_row(6, "jpn", true)],
    );
    assert_eq!(
        selected_label(&embedded_only),
        Some((SubtitleFileOrigin::Embedded, "jpn".to_owned()))
    );

    // Ни language, ни default: первая в списке.
    let mut plain = owner_with_config(&config_for("de"));
    plain.begin_media(instance(4), SubtitleMediaSource::NotLocal);
    plain.sync_embedded_tracks(
        Some(instance(4)),
        &[srt_row(5, "eng", false), srt_row(6, "jpn", false)],
    );
    assert_eq!(
        plain.menu_view().tracks.iter().position(|t| t.is_selected),
        Some(0)
    );
}

#[test]
fn embedded_tracks_appearing_later_do_not_override_the_users_choice() {
    let (_dir, video) = movie_with(&[("Movie.en.srt", "1\n00:00:01,000 --> 00:00:02,000\nSide\n")]);
    let media = instance(2);
    let mut owner = owner_with_config(&config_for("ru"));
    begin_and_wait(&mut owner, media, &video);
    let side_id = owner.menu_view().tracks[0].id;
    assert_eq!(owner.select_track(side_id), SelectTrackOutcome::Selected);

    // Появилась русская дорожка, которая при автовыборе победила бы.
    owner.sync_embedded_tracks(Some(media), &[srt_row(6, "rus", true)]);

    assert_eq!(
        selected_label(&owner),
        Some((SubtitleFileOrigin::NextToVideo, "en".to_owned())),
        "the user's choice stays"
    );
}

#[test]
fn forced_embedded_track_of_the_preferred_language_is_shown_even_when_subtitles_are_off() {
    let media = instance(2);
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let russian = fixture.subtitle_row(1).id;
    let mut config = config_for("ru");
    config.enabled = false;
    let mut owner = embedded_owner(&config, media, &fixture);
    owner.deliver_feed_messages(fixture.feed_messages(media));

    // Субтитры выключены, но forced русская дорожка показывается.
    assert!(visible_texts(&mut owner, media, 1_000) > 0);
    assert_eq!(cached_cues(&owner, russian), 2);
}
