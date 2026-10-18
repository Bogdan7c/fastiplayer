//! PGS при выключенных субтитрах (решение владельца 14 и сессия 11, как у проигрывателей
//! Blu-ray): без настоящей forced-дорожки показываются только forced-ОБЪЕКТЫ дорожки языка
//! предпочтений. Фикстура `pgs-basic.mkv`: обычная картинка 0.5–1.5 с, forced-картинка (вверху)
//! 2.0–2.6 с. Тесты идут по настоящей цепочке «пакеты -> владелец -> поток-рисовальщик ->
//! overlay», пиксели на GPU — в `embedded_pgs_gpu`.

use media_core::{TrackDisposition, TrackLabels, TrackLanguage};

use super::embedded_pgs::*;
use super::embedded_support::*;
use super::*;
use crate::state::subtitles::intents::SubtitleVisibility;
use crate::state::subtitles::media_state::{DisplayChoice, DisplayMode};

/// Момент обычной (не forced) картинки.
const REGULAR_PICTURE_MS: u64 = 1_000;
/// Момент forced-картинки.
const FORCED_PICTURE_MS: u64 = 2_300;
/// Момент без картинок.
const NO_PICTURE_MS: u64 = 1_700;

/// Конфиг «выключено» с предпочтением языка.
fn hidden_config(languages: &str) -> SubtitlesConfig {
    SubtitlesConfig {
        enabled: false,
        preferred_languages: languages.to_owned(),
        ..SubtitlesConfig::default()
    }
}

/// Владелец с одной PGS-дорожкой (`eng`), все пакеты доставлены.
fn pgs_owner_with_packets(config: &SubtitlesConfig) -> (SubtitleOwner, MediaInstanceId) {
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    let mut owner = embedded_owner(config, media, &fixture);
    owner.deliver_feed_messages(fixture.feed_messages(media));
    (owner, media)
}

fn display_choice(owner: &SubtitleOwner) -> Option<DisplayChoice> {
    owner
        .media
        .as_ref()
        .and_then(|media| media.display_choice())
}

#[test]
fn hidden_subtitles_show_only_forced_objects_of_the_preferred_language_track() {
    let (mut owner, media) = pgs_owner_with_packets(&hidden_config("en"));

    assert_eq!(
        display_choice(&owner).map(|choice| choice.mode),
        Some(DisplayMode::ForcedPicturesOnly)
    );
    assert!(
        !shows_picture(&mut owner, media, REGULAR_PICTURE_MS),
        "the regular picture must stay hidden"
    );
    assert!(
        shows_picture(&mut owner, media, FORCED_PICTURE_MS),
        "the forced picture is shown"
    );
    assert!(!shows_picture(&mut owner, media, NO_PICTURE_MS));
}

#[test]
fn shown_subtitles_show_regular_and_forced_objects() {
    let (mut owner, media) = pgs_owner_with_packets(&hidden_config("en"));
    owner.set_visibility(SubtitleVisibility::Shown);

    assert_eq!(
        display_choice(&owner).map(|choice| choice.mode),
        Some(DisplayMode::AllCues)
    );
    assert!(shows_picture(&mut owner, media, REGULAR_PICTURE_MS));
    assert!(shows_picture(&mut owner, media, FORCED_PICTURE_MS));
    assert!(!shows_picture(&mut owner, media, NO_PICTURE_MS));
}

#[test]
fn toggling_visibility_switches_the_presentation_without_a_stale_picture() {
    let (mut owner, media) = pgs_owner_with_packets(&hidden_config("en"));
    assert!(!shows_picture(&mut owner, media, REGULAR_PICTURE_MS));

    // Включили на кадре с обычной картинкой: она сразу видна.
    owner.set_visibility(SubtitleVisibility::Shown);
    assert!(shows_picture(&mut owner, media, REGULAR_PICTURE_MS));
    // Выключили на том же кадре: обычная картинка не должна остаться на экране.
    owner.set_visibility(SubtitleVisibility::Hidden);
    assert!(
        !shows_picture(&mut owner, media, REGULAR_PICTURE_MS),
        "no stale regular picture after switching off"
    );
    // Forced-картинка при этом остаётся в обоих режимах.
    assert!(shows_picture(&mut owner, media, FORCED_PICTURE_MS));
    owner.set_visibility(SubtitleVisibility::Shown);
    assert!(shows_picture(&mut owner, media, FORCED_PICTURE_MS));
}

#[test]
fn language_mismatch_gives_no_forced_objects_fallback() {
    let (mut owner, media) = pgs_owner_with_packets(&hidden_config("ru"));

    assert_eq!(display_choice(&owner), None);
    assert!(!shows_picture(&mut owner, media, FORCED_PICTURE_MS));
    assert!(!shows_picture(&mut owner, media, REGULAR_PICTURE_MS));
}

#[test]
fn a_container_forced_track_wins_over_the_forced_objects_fallback() {
    let media = instance(2);
    let (mut fixture, regular_track, forced_track) = fixture_with_second_pgs_track();
    // Вторая дорожка помечена контейнером как forced, того же языка.
    let forced_row = fixture
        .tracks
        .iter_mut()
        .find(|row| row.id == forced_track)
        .expect("second track row");
    forced_row.labels = TrackLabels {
        language: Some(TrackLanguage::new("eng")),
        title: None,
        disposition: TrackDisposition {
            is_forced: true,
            ..TrackDisposition::default()
        },
    };
    let mut owner = embedded_owner(&hidden_config("en"), media, &fixture);
    for track in [regular_track, forced_track] {
        owner.deliver_feed_messages(blocks(
            &fixture,
            media,
            track,
            0,
            &[FIRST_PICTURE, FIRST_CLEAR, SECOND_PICTURE, SECOND_CLEAR],
        ));
    }

    let choice = display_choice(&owner).expect("a track is displayed");
    assert_eq!(choice.track_id, owner_track_id(&owner, forced_track));
    assert_eq!(
        choice.mode,
        DisplayMode::AllCues,
        "the forced track is shown whole, forced-objects mode is not used"
    );
    assert!(
        shows_picture(&mut owner, media, REGULAR_PICTURE_MS),
        "everything of a container-forced track is shown, even without the object flag"
    );
}

#[test]
fn forced_view_follows_a_late_successor_that_shortens_the_forced_picture() {
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    let track = fixture.subtitle_row(0).id;
    let mut owner = embedded_owner(&hidden_config("en"), media, &fixture);
    owner.deliver_feed_messages(blocks(
        &fixture,
        media,
        track,
        0,
        &[FIRST_PICTURE, FIRST_CLEAR, SECOND_PICTURE],
    ));
    assert!(
        shows_picture(&mut owner, media, 2_900),
        "the open forced picture stays until a successor arrives"
    );

    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[SECOND_CLEAR]));

    assert!(shows_picture(&mut owner, media, FORCED_PICTURE_MS));
    assert!(
        !shows_picture(&mut owner, media, 2_900),
        "shortening is applied to the forced-only timeline too"
    );
    let counters = owner.media.as_ref().expect("media").embedded_pgs.counters();
    assert_eq!(counters.timeline_rebuilds, 0, "no rebuild was needed");
    assert_eq!(counters.shortening_mismatches, 0);
}

#[test]
fn forced_view_survives_a_seek_rebuild_and_stays_forced_only() {
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    let track = fixture.subtitle_row(0).id;
    let mut owner = embedded_owner(&hidden_config("en"), media, &fixture);
    owner.deliver_feed_messages(blocks(
        &fixture,
        media,
        track,
        0,
        &[FIRST_PICTURE, FIRST_CLEAR, SECOND_PICTURE, SECOND_CLEAR],
    ));
    // Бюджет меньше хранилища и позиция у forced-картинки: политика памяти пересоберёт шкалы.
    observe_position(&mut owner, media, FORCED_PICTURE_MS);
    owner.settings.embedded_limits.pgs = owner
        .settings
        .embedded_limits
        .pgs
        .with_shown_budget_bytes(1);
    owner.deliver_feed_messages(blocks(&fixture, media, track, 1, &[FIRST_PICTURE]));

    let counters = owner.media.as_ref().expect("media").embedded_pgs.counters();
    assert!(
        counters.timeline_rebuilds >= 1,
        "the rebuild really happened"
    );
    assert!(
        !shows_picture(&mut owner, media, REGULAR_PICTURE_MS),
        "regular picture never leaks through the rebuilt forced view"
    );
    assert!(shows_picture(&mut owner, media, FORCED_PICTURE_MS));
}

#[test]
fn sync_offset_moves_forced_only_pictures_like_the_full_timeline() {
    use crate::state::subtitles::sync_offset::SyncNudge;

    let (mut owner, media) = pgs_owner_with_packets(&hidden_config("en"));
    assert!(shows_picture(&mut owner, media, FORCED_PICTURE_MS));
    // Субтитры позже на 1 с (10 шагов по 0.1 с): картинка 2.0-2.6 с приходит на 3.0-3.6 с.
    for _ in 0..10 {
        owner.nudge_sync(SyncNudge::Later);
    }

    assert!(!shows_picture(&mut owner, media, FORCED_PICTURE_MS));
    assert!(shows_picture(&mut owner, media, 3_300));
}
