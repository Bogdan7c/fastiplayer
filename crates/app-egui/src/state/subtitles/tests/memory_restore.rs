//! Память выбора на стороне владельца субтитров: восстановление при старте media и захват
//! ТОЛЬКО действий пользователя. Дорожки берутся из настоящего файла-фикстуры.

use playlist_state::{RememberedFilePath, RememberedSubtitleChoice, RememberedTrackHint};
use subtitle_core::SubtitleSyncOffset;

use super::super::menu_view::SubtitleFileOrigin;
use super::embedded_support::*;
use super::track_selection::{movie_with, settle};
use super::*;
use crate::state::subtitles::{SubtitleRestoreRequest, SubtitleVisibility, SyncNudge};

const FIXTURE: &str = "two-audio-two-srt.mkv";
const SRT_CODEC: &str = "S_TEXT/UTF8";

fn english_hint() -> RememberedTrackHint {
    RememberedTrackHint::new(
        Some("eng".to_owned()),
        Some("English".to_owned()),
        SRT_CODEC.to_owned(),
        0,
    )
    .expect("hint")
}

fn russian_hint() -> RememberedTrackHint {
    RememberedTrackHint::new(
        Some("rus".to_owned()),
        Some("Russian SDH".to_owned()),
        SRT_CODEC.to_owned(),
        1,
    )
    .expect("hint")
}

fn restore(choice: RememberedSubtitleChoice) -> SubtitleRestoreRequest {
    SubtitleRestoreRequest {
        choice: Some(choice),
        sync_offset: SubtitleSyncOffset::ZERO,
    }
}

fn disabled_config() -> SubtitlesConfig {
    SubtitlesConfig {
        enabled: false,
        ..SubtitlesConfig::default()
    }
}

/// Владелец, у которого media без файла рядом, а дорожки приходят из снимка плеера.
fn embedded_owner_restoring(
    config: &SubtitlesConfig,
    media: MediaInstanceId,
    fixture: &DemuxedFixture,
    request: SubtitleRestoreRequest,
) -> SubtitleOwner {
    let mut owner = owner_with_config(config);
    owner.begin_media_restoring(media, SubtitleMediaSource::NotLocal, request);
    owner.sync_embedded_tracks(Some(media), &fixture.tracks);
    wait_until(&mut owner, "rasterizer", |owner| {
        owner.presenter.has_rasterizer()
    });
    owner
}

/// Язык отмеченной (выбранной и включённой) строки меню.
fn selected_language(owner: &SubtitleOwner) -> Option<String> {
    owner
        .menu_view()
        .tracks
        .into_iter()
        .find(|track| track.is_selected)
        .map(|track| {
            track
                .label
                .language
                .map(|language| language.as_str().to_owned())
                .unwrap_or_default()
        })
}

#[test]
fn remembered_embedded_track_is_selected_although_global_subtitles_are_off() {
    let fixture = demux_fixture(FIXTURE);

    let without_memory = embedded_owner_restoring(
        &disabled_config(),
        instance(2),
        &fixture,
        SubtitleRestoreRequest::nothing_remembered(),
    );
    let mut with_memory = embedded_owner_restoring(
        &disabled_config(),
        instance(2),
        &fixture,
        restore(RememberedSubtitleChoice::Embedded(russian_hint())),
    );

    assert_eq!(
        without_memory.menu_view().visibility,
        SubtitleVisibility::Hidden
    );
    assert_eq!(
        selected_language(&without_memory),
        None,
        "control: nothing shown"
    );
    assert_eq!(
        with_memory.menu_view().visibility,
        SubtitleVisibility::Shown
    );
    assert_eq!(selected_language(&with_memory).as_deref(), Some("rus"));
    assert_eq!(
        with_memory.take_enabled_preference_change(),
        None,
        "restoring from memory must not touch the global `subtitles.enabled`"
    );
    assert_eq!(
        with_memory.take_memory_changes(),
        None,
        "restoring is not a user choice and is not written back"
    );
}

#[test]
fn remembered_off_hides_subtitles_although_global_subtitles_are_on() {
    let fixture = demux_fixture(FIXTURE);

    let automatic = embedded_owner_restoring(
        &enabled_config(),
        instance(2),
        &fixture,
        SubtitleRestoreRequest::nothing_remembered(),
    );
    let mut remembered_off = embedded_owner_restoring(
        &enabled_config(),
        instance(2),
        &fixture,
        restore(RememberedSubtitleChoice::Off),
    );

    assert_eq!(
        selected_language(&automatic).as_deref(),
        Some("eng"),
        "control"
    );
    assert_eq!(
        remembered_off.menu_view().visibility,
        SubtitleVisibility::Hidden
    );
    assert_eq!(selected_language(&remembered_off), None);
    assert_eq!(remembered_off.take_enabled_preference_change(), None);
    assert_eq!(remembered_off.take_memory_changes(), None);
    // «Выключено» действует на это видео; включить обратно можно клавишей как обычно.
    assert_eq!(
        remembered_off.toggle_visibility(),
        SubtitleVisibility::Shown
    );
    assert_eq!(selected_language(&remembered_off).as_deref(), Some("eng"));
}

#[test]
fn remembered_sync_offset_is_applied_to_the_media_and_to_its_cues() {
    let fixture = demux_fixture(FIXTURE);
    let request = SubtitleRestoreRequest {
        choice: Some(RememberedSubtitleChoice::Embedded(english_hint())),
        sync_offset: SubtitleSyncOffset::from_millis(500),
    };
    let media = instance(2);

    let mut owner = embedded_owner_restoring(&disabled_config(), media, &fixture, request);
    owner.deliver_feed_messages(fixture.feed_messages(media));

    assert_eq!(
        owner.menu_view().sync_offset,
        SubtitleSyncOffset::from_millis(500)
    );
    // Реплика «Hello» идёт 0,5-1,5 с; со сдвигом +0,5 с она показывается 1,0-2,0 с.
    assert_eq!(visible_texts(&mut owner, media, 700), 0, "not yet at 0.7 s");
    assert!(
        visible_texts(&mut owner, media, 1_800) > 0,
        "still shown at 1.8 s"
    );
    assert_eq!(
        owner.take_memory_changes(),
        None,
        "the restored offset is not a user change"
    );
}

#[test]
fn a_remembered_track_that_vanished_falls_back_to_automatic_selection_without_error() {
    let fixture = demux_fixture(FIXTURE);
    let vanished = RememberedTrackHint::new(
        Some("jpn".to_owned()),
        Some("Japanese".to_owned()),
        SRT_CODEC.to_owned(),
        2,
    )
    .expect("hint");

    let mut owner = embedded_owner_restoring(
        &enabled_config(),
        instance(2),
        &fixture,
        restore(RememberedSubtitleChoice::Embedded(vanished)),
    );

    assert_eq!(
        selected_language(&owner).as_deref(),
        Some("eng"),
        "automatic choice"
    );
    assert!(
        owner
            .menu_view()
            .tracks
            .iter()
            .all(|track| track.availability == SubtitleTrackAvailability::Available),
        "no unavailable track and no error row appears"
    );
    assert_eq!(owner.take_memory_changes(), None);
}

#[test]
fn a_user_action_before_the_tracks_arrive_drops_the_remembered_choice() {
    let fixture = demux_fixture(FIXTURE);
    let media = instance(2);
    let mut owner = owner_with_config(&enabled_config());
    owner.begin_media_restoring(
        media,
        SubtitleMediaSource::NotLocal,
        restore(RememberedSubtitleChoice::Embedded(russian_hint())),
    );

    // Пользователь выключает субтитры, пока дорожки ещё не пришли из плеера.
    owner.set_visibility(SubtitleVisibility::Hidden);
    owner.sync_embedded_tracks(Some(media), &fixture.tracks);

    assert_eq!(owner.menu_view().visibility, SubtitleVisibility::Hidden);
    assert_eq!(
        selected_language(&owner),
        None,
        "the remembered Russian track must not override the user's action"
    );
}

#[test]
fn only_user_actions_are_reported_for_memory_and_automatic_selection_never_is() {
    let fixture = demux_fixture(FIXTURE);
    let media = instance(2);
    let mut owner = embedded_owner_restoring(
        &enabled_config(),
        media,
        &fixture,
        SubtitleRestoreRequest::nothing_remembered(),
    );
    assert_eq!(
        selected_language(&owner).as_deref(),
        Some("eng"),
        "auto-selected"
    );
    assert_eq!(
        owner.take_memory_changes(),
        None,
        "automatic selection is not saved"
    );

    let russian = owner.menu_view().tracks[1].id;
    owner.select_track(russian);
    let first = owner.take_memory_changes().expect("a user choice");
    assert_eq!(first.media_instance_id, media);
    assert_eq!(
        first.choice,
        Some(RememberedSubtitleChoice::Embedded(russian_hint()))
    );
    assert_eq!(first.sync_offset, None);
    assert_eq!(owner.take_memory_changes(), None, "reported exactly once");

    owner.nudge_sync(SyncNudge::Later);
    owner.nudge_sync(SyncNudge::Later);
    let offset_only = owner.take_memory_changes().expect("a user offset");
    assert_eq!(offset_only.choice, None, "the choice did not change");
    assert_eq!(
        offset_only.sync_offset,
        Some(SubtitleSyncOffset::from_millis(200))
    );

    owner.set_visibility(SubtitleVisibility::Hidden);
    assert_eq!(
        owner.take_memory_changes().expect("off").choice,
        Some(RememberedSubtitleChoice::Off)
    );
}

#[test]
fn the_pending_choice_survives_until_its_track_exists_and_then_applies_once() {
    let fixture = demux_fixture(FIXTURE);
    let media = instance(2);
    let mut owner = owner_with_config(&disabled_config());
    owner.begin_media_restoring(
        media,
        SubtitleMediaSource::NotLocal,
        restore(RememberedSubtitleChoice::Embedded(russian_hint())),
    );
    assert_eq!(
        selected_language(&owner),
        None,
        "no tracks yet: nothing to select"
    );

    owner.sync_embedded_tracks(Some(media), &fixture.tracks);

    assert_eq!(selected_language(&owner).as_deref(), Some("rus"));
    // Позднее обновление списка не возвращает уже применённое ожидание и не перебивает выбор.
    owner.sync_embedded_tracks(Some(media), &fixture.tracks);
    assert_eq!(selected_language(&owner).as_deref(), Some("rus"));
}

// ---- файлы субтитров ----

fn restoring_local_owner(
    config: &SubtitlesConfig,
    media: MediaInstanceId,
    video: &Path,
    request: SubtitleRestoreRequest,
) -> SubtitleOwner {
    let mut owner = owner_with_config(config);
    owner.begin_media_restoring(
        media,
        SubtitleMediaSource::LocalFile(video.to_path_buf()),
        request,
    );
    settle(&mut owner);
    owner
}

fn file_choice(path: &Path) -> RememberedSubtitleChoice {
    RememberedSubtitleChoice::File(RememberedFilePath::new(path.to_path_buf()).expect("path"))
}

const ENGLISH_SRT: &str = "1\n00:00:01,000 --> 00:00:02,000\nEnglish side\n";
const RUSSIAN_SRT: &str = "1\n00:00:01,000 --> 00:00:02,000\nRussian side\n";

#[test]
fn a_remembered_file_next_to_the_video_is_selected_instead_of_the_automatic_choice() {
    let (directory, video) =
        movie_with(&[("Movie.en.srt", ENGLISH_SRT), ("Movie.ru.srt", RUSSIAN_SRT)]);
    let russian = directory.path().join("Movie.ru.srt");

    let mut owner = restoring_local_owner(
        &SubtitlesConfig {
            enabled: true,
            preferred_languages: "en".to_owned(),
            ..SubtitlesConfig::default()
        },
        instance(2),
        &video,
        restore(file_choice(&russian)),
    );

    assert_eq!(
        selected_language(&owner).as_deref(),
        Some("ru"),
        "memory beats the language list"
    );
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Ready);
    assert_eq!(owner.take_memory_changes(), None);
}

#[test]
fn a_remembered_file_that_was_deleted_falls_back_to_automatic_selection_silently() {
    let (directory, video) = movie_with(&[("Movie.en.srt", ENGLISH_SRT)]);
    let deleted = directory.path().join("Movie.ru.srt");

    let mut owner = restoring_local_owner(
        &enabled_config(),
        instance(2),
        &video,
        restore(file_choice(&deleted)),
    );

    assert_eq!(
        selected_language(&owner).as_deref(),
        Some("en"),
        "automatic choice"
    );
    let menu = owner.menu_view();
    assert_eq!(
        menu.tracks.len(),
        1,
        "no placeholder row for the missing file"
    );
    assert_eq!(
        menu.tracks[0].availability,
        SubtitleTrackAvailability::Available
    );
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Ready);
    assert!(owner.unavailable_failure().is_none(), "no error state");
    assert_eq!(owner.take_memory_changes(), None);
}

#[test]
fn a_remembered_file_from_another_folder_is_attached_when_it_still_exists() {
    let (_video_directory, video) = movie_with(&[]);
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let dropped = elsewhere.path().join("Downloaded.ru.srt");
    std::fs::write(&dropped, RUSSIAN_SRT).expect("write");

    let mut owner = restoring_local_owner(
        &disabled_config(),
        instance(2),
        &video,
        restore(file_choice(&dropped)),
    );

    let menu = owner.menu_view();
    assert_eq!(menu.tracks.len(), 1);
    assert_eq!(
        menu.tracks[0].label.origin,
        SubtitleFileOrigin::ChosenByUser,
        "attached as a user-chosen file"
    );
    assert!(menu.tracks[0].is_selected);
    assert_eq!(menu.visibility, SubtitleVisibility::Shown);
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Ready);
    assert_eq!(owner.take_enabled_preference_change(), None);
    assert_eq!(owner.take_memory_changes(), None);
}

#[test]
fn a_web_source_checks_the_remembered_file_in_the_background_too() {
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let dropped = elsewhere.path().join("Extra.en.srt");
    std::fs::write(&dropped, ENGLISH_SRT).expect("write");
    let missing = elsewhere.path().join("Gone.en.srt");
    let media = instance(2);

    let mut existing = owner_with_config(&disabled_config());
    existing.begin_media_restoring(
        media,
        SubtitleMediaSource::NotLocal,
        restore(file_choice(&dropped)),
    );
    settle(&mut existing);
    let mut absent = owner_with_config(&disabled_config());
    absent.begin_media_restoring(
        media,
        SubtitleMediaSource::NotLocal,
        restore(file_choice(&missing)),
    );
    settle(&mut absent);

    assert_eq!(existing.menu_view().tracks.len(), 1);
    assert!(existing.menu_view().tracks[0].is_selected);
    assert!(
        absent.menu_view().tracks.is_empty(),
        "a missing file adds nothing"
    );
    assert_eq!(
        absent.lifecycle_view(),
        SubtitleLifecycleView::NoSubtitleFile
    );
}

#[test]
fn attaching_a_file_is_reported_as_a_user_choice_with_its_path() {
    let (_directory, video) = movie_with(&[]);
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let dropped = elsewhere.path().join("Extra.en.srt");
    std::fs::write(&dropped, ENGLISH_SRT).expect("write");
    let media = instance(2);
    let mut owner = restoring_local_owner(
        &disabled_config(),
        media,
        &video,
        SubtitleRestoreRequest::nothing_remembered(),
    );

    owner.attach_file(&dropped);

    let changes = owner.take_memory_changes().expect("user attached a file");
    assert_eq!(changes.media_instance_id, media);
    assert_eq!(changes.choice, Some(file_choice(&dropped)));
}

#[test]
fn a_file_dropped_together_with_the_video_beats_the_remembered_choice() {
    let (_directory, video) = movie_with(&[]);
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let dropped = elsewhere.path().join("Dropped.en.srt");
    std::fs::write(&dropped, ENGLISH_SRT).expect("write");
    let media = instance(2);
    let mut owner = owner_with_config(&disabled_config());
    owner.queue_file_for_next_media(dropped.clone());

    owner.begin_media_restoring(
        media,
        SubtitleMediaSource::LocalFile(video),
        restore(RememberedSubtitleChoice::Off),
    );
    settle(&mut owner);

    assert_eq!(
        owner.menu_view().visibility,
        SubtitleVisibility::Shown,
        "the dropped file wins over Off"
    );
    assert!(owner.menu_view().tracks[0].is_selected);
    assert_eq!(
        owner
            .take_memory_changes()
            .expect("dropped with the video is a user choice")
            .choice,
        Some(file_choice(&dropped))
    );
}
