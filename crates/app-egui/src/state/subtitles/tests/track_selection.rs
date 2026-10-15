//! Выбор дорожек, автовыбор, видимость, сдвиг, добавление файлов и настройки: сквозные
//! тесты владельца. Все сценарии идут через настоящий `begin_media` и фоновую задачу, а
//! результат проверяется по тому, что реально увидит кадр: overlay и тексты активных реплик.

use fastiplayer_config::SubtitlesConfig;
use subtitle_core::{SubtitleContent, SubtitleSyncOffset};

use super::super::intents::{
    AttachOutcome, CycleOutcome, QueueOutcome, SelectTrackOutcome, SubtitleVisibility,
};
use super::super::menu_view::{
    SubtitleMenuView, SubtitleTrackAvailability, SubtitleUnavailableReason,
};
use super::super::sync_offset::SyncNudge;
use super::super::track_list::TrackLoadState;
use super::*;

const GENERATION: u64 = 3;
const VIEW: (u32, u32) = (1280, 720);

/// Английская дорожка: реплики [1 с, 3 с) и [5 с, 7 с).
const ENGLISH_SRT: &str = "1\n00:00:01,000 --> 00:00:03,000\nEnglish line\n\n\
2\n00:00:05,000 --> 00:00:07,000\nEnglish again\n";
/// Русская дорожка: одна реплика [2 с, 4 с).
pub(super) const RUSSIAN_SRT: &str = "1\n00:00:02,000 --> 00:00:04,000\nРусская строка\n";
/// Принудительная русская дорожка: реплика [2 с, 3 с).
const FORCED_RUSSIAN_SRT: &str = "1\n00:00:02,000 --> 00:00:03,000\nТолько по-русски\n";
/// Одна реплика, начинающаяся ровно на 10,0 с.
const LATE_CUE_SRT: &str = "1\n00:00:10,000 --> 00:00:12,000\nLate cue\n";

fn config_with_languages(enabled: bool, languages: &str) -> SubtitlesConfig {
    SubtitlesConfig {
        enabled,
        preferred_languages: languages.to_owned(),
        ..SubtitlesConfig::default()
    }
}

/// Каталог с `Movie.mkv` и файлами субтитров `(имя, содержимое)`.
pub(super) fn movie_with(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), None);
    for (name, content) in files {
        std::fs::write(directory.path().join(name), content).expect("write subtitle file");
    }
    (directory, video)
}

/// Ждёт, пока все фоновые задачи владельца завершатся и применятся.
pub(super) fn settle(owner: &mut SubtitleOwner) {
    let deadline = Instant::now() + LOAD_TIMEOUT;
    loop {
        owner.poll_jobs();
        let busy = owner
            .media
            .as_ref()
            .is_some_and(super::super::media_state::MediaSubtitles::has_running_jobs);
        if !busy {
            return;
        }
        assert!(Instant::now() < deadline, "subtitle jobs did not settle");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn frame_rect() -> PictureRect {
    full_rect(VIEW.0, VIEW.1)
}

/// Есть ли overlay на кадре с `pts_millis` (media 1, поколение `GENERATION`).
pub(super) fn overlay_present(
    owner: &mut SubtitleOwner,
    media: MediaInstanceId,
    pts_millis: u64,
) -> bool {
    owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(pts_millis, GENERATION, frame_rect()),
        )
        .is_some()
}

/// Тексты реплик показываемой дорожки, активные при воспроизведении на `pts_millis`.
fn active_texts(owner: &SubtitleOwner, pts_millis: u64) -> Vec<String> {
    let Some(media) = &owner.media else {
        return Vec::new();
    };
    let Some(entry) = media.display_track_id().and_then(|id| media.tracks.get(id)) else {
        return Vec::new();
    };
    let TrackLoadState::Ready(track) = &entry.load else {
        return Vec::new();
    };
    let lookup = track.lookup_time_for(Duration::from_millis(pts_millis));
    let mut ids = Vec::new();
    track.timeline().collect_active_at(lookup, &mut ids);
    ids.into_iter()
        .filter_map(|id| track.timeline().event(id))
        .filter_map(|event| match event.content() {
            SubtitleContent::Text(cue) => Some(cue.text.plain_text()),
            _ => None,
        })
        .collect()
}

/// Сколько дорожек реально прочитано (лениво читается только показываемая).
fn loaded_track_count(owner: &SubtitleOwner) -> usize {
    owner.media.as_ref().map_or(0, |media| {
        media
            .tracks
            .entries()
            .iter()
            .filter(|entry| matches!(entry.load, TrackLoadState::Ready(_)))
            .count()
    })
}

/// Id дорожки по имени файла.
fn track_named(view: &SubtitleMenuView, file_name: &str) -> subtitle_core::SubtitleTrackId {
    view.tracks
        .iter()
        .find(|track| track.label.file_name.as_deref() == Some(file_name))
        .unwrap_or_else(|| panic!("track {file_name} is in the menu"))
        .id
}

fn selected_file_name(view: &SubtitleMenuView) -> Option<String> {
    view.tracks
        .iter()
        .find(|track| track.is_selected)
        .and_then(|track| track.label.file_name.clone())
}

#[test]
fn preferred_uk_then_ru_picks_the_russian_track_among_en_and_ru() {
    let (_dir, video) = movie_with(&[("Movie.en.srt", ENGLISH_SRT), ("Movie.ru.srt", RUSSIAN_SRT)]);
    let mut owner = owner_with_config(&config_with_languages(true, "uk, ru"));
    begin_and_wait(&mut owner, instance(1), &video);

    let view = owner.menu_view();
    assert_eq!(view.tracks.len(), 2, "both files are offered as tracks");
    assert_eq!(selected_file_name(&view).as_deref(), Some("Movie.ru.srt"));
    assert_eq!(active_texts(&owner, 2_500), vec!["Русская строка"]);
    assert!(overlay_present(&mut owner, instance(1), 2_500));
    assert_eq!(
        loaded_track_count(&owner),
        1,
        "only the shown track is read"
    );
}

#[test]
fn subtitles_are_off_by_default_and_nothing_is_read_or_drawn() {
    let (_dir, video) = movie_with(&[("Movie.en.srt", ENGLISH_SRT)]);
    let mut owner = owner_with_config(&SubtitlesConfig::default());
    begin_and_wait(&mut owner, instance(1), &video);

    let view = owner.menu_view();
    assert!(view.has_tracks, "the track is offered in the menu");
    assert_eq!(view.visibility, SubtitleVisibility::Hidden);
    assert!(view.tracks.iter().all(|track| !track.is_selected));
    assert!(!overlay_present(&mut owner, instance(1), 2_000));
    assert_eq!(loaded_track_count(&owner), 0, "no file is read while off");
    assert_eq!(
        owner.take_enabled_preference_change(),
        None,
        "automatic state is not a user preference change"
    );
}

#[test]
fn disabled_subtitles_still_show_a_forced_track_of_the_preferred_language() {
    let (_dir, video) = movie_with(&[
        ("Movie.en.srt", ENGLISH_SRT),
        ("Movie.ru.forced.srt", FORCED_RUSSIAN_SRT),
    ]);
    let mut owner = owner_with_config(&config_with_languages(false, "ru"));
    begin_and_wait(&mut owner, instance(1), &video);

    assert_eq!(active_texts(&owner, 2_500), vec!["Только по-русски"]);
    assert!(overlay_present(&mut owner, instance(1), 2_500));
    assert_eq!(
        owner.menu_view().visibility,
        SubtitleVisibility::Hidden,
        "the user's switch stays off"
    );

    // Включение переходит на полную дорожку: forced-вариант не мешает выбору.
    owner.set_visibility(SubtitleVisibility::Shown);
    settle(&mut owner);
    assert_eq!(
        selected_file_name(&owner.menu_view()).as_deref(),
        Some("Movie.en.srt")
    );
    assert_eq!(active_texts(&owner, 2_000), vec!["English line"]);
}

#[test]
fn disabled_subtitles_without_a_matching_forced_track_show_nothing() {
    let (_dir, video) = movie_with(&[
        ("Movie.ru.srt", RUSSIAN_SRT),
        ("Movie.en.forced.srt", ENGLISH_SRT),
    ]);
    let mut owner = owner_with_config(&config_with_languages(false, "ru"));
    begin_and_wait(&mut owner, instance(1), &video);

    assert!(!overlay_present(&mut owner, instance(1), 2_500));
    assert!(!overlay_present(&mut owner, instance(1), 2_000));
    assert_eq!(loaded_track_count(&owner), 0);
}

#[test]
fn toggle_off_removes_the_overlay_of_the_same_frame_and_toggle_on_brings_it_back() {
    let media = instance(1);
    let (mut owner, _dir) = ready_owner(media);
    assert!(overlay_present(&mut owner, media, 2_000), "control: shown");

    assert_eq!(owner.toggle_visibility(), SubtitleVisibility::Hidden);
    assert!(
        !overlay_present(&mut owner, media, 2_000),
        "same frame, off"
    );
    assert_eq!(
        owner.take_enabled_preference_change(),
        Some(SubtitleVisibility::Hidden)
    );
    assert_eq!(
        owner.take_enabled_preference_change(),
        None,
        "reported once"
    );

    assert_eq!(owner.toggle_visibility(), SubtitleVisibility::Shown);
    assert!(overlay_present(&mut owner, media, 2_000), "same frame, on");
    assert_eq!(
        owner.take_enabled_preference_change(),
        Some(SubtitleVisibility::Shown)
    );

    // Повторная установка текущего значения не считается изменением.
    owner.set_visibility(SubtitleVisibility::Shown);
    assert_eq!(owner.take_enabled_preference_change(), None);
}

#[test]
fn selecting_another_track_changes_what_the_frame_shows() {
    let (_dir, video) = movie_with(&[("Movie.en.srt", ENGLISH_SRT), ("Movie.ru.srt", RUSSIAN_SRT)]);
    let mut owner = owner_with_config(&config_with_languages(true, "ru"));
    let media = instance(1);
    begin_and_wait(&mut owner, media, &video);
    let view = owner.menu_view();
    let english = track_named(&view, "Movie.en.srt");
    let russian = track_named(&view, "Movie.ru.srt");

    // 3,5 с: русская реплика ещё идёт, английской уже нет.
    assert!(overlay_present(&mut owner, media, 3_500));
    assert_eq!(owner.select_track(english), SelectTrackOutcome::Selected);
    settle(&mut owner);
    assert!(
        !overlay_present(&mut owner, media, 3_500),
        "English has no cue at 3.5 s"
    );
    assert!(
        overlay_present(&mut owner, media, 6_000),
        "English cue at 6 s"
    );
    assert_eq!(active_texts(&owner, 6_000), vec!["English again"]);

    assert_eq!(owner.select_track(russian), SelectTrackOutcome::Selected);
    settle(&mut owner);
    assert!(overlay_present(&mut owner, media, 3_500));
    assert!(!overlay_present(&mut owner, media, 6_000));
}

#[test]
fn rapid_track_switching_keeps_only_the_last_choice_and_drops_the_cancelled_read() {
    let (_dir, video) = movie_with(&[("Movie.en.srt", ENGLISH_SRT), ("Movie.ru.srt", RUSSIAN_SRT)]);
    let mut owner = owner_with_config(&config_with_languages(false, ""));
    let media = instance(1);
    begin_and_wait(&mut owner, media, &video);
    let view = owner.menu_view();
    let english = track_named(&view, "Movie.en.srt");
    let russian = track_named(&view, "Movie.ru.srt");

    owner.select_track(russian);
    owner.select_track(english);
    settle(&mut owner);

    assert_eq!(active_texts(&owner, 2_000), vec!["English line"]);
    assert_eq!(
        loaded_track_count(&owner),
        1,
        "the superseded read left no track behind"
    );
    let russian_state = &owner
        .media
        .as_ref()
        .expect("media")
        .tracks
        .get(russian)
        .expect("track")
        .load;
    assert!(matches!(russian_state, TrackLoadState::NotLoaded));
}

#[test]
fn unknown_track_is_reported_and_changes_nothing() {
    let media = instance(1);
    let (mut owner, _dir) = ready_owner(media);
    let before = owner.menu_view();
    let outcome = owner.select_track(subtitle_core::SubtitleTrackId::new(999));
    assert_eq!(outcome, SelectTrackOutcome::UnknownTrack);
    assert_eq!(owner.menu_view(), before);
    // Без media выбирать нечего.
    let mut empty = super::owner();
    assert_eq!(
        empty.select_track(subtitle_core::SubtitleTrackId::new(0)),
        SelectTrackOutcome::UnknownTrack
    );
}

#[test]
fn next_track_cycles_through_tracks_then_off_then_the_first_again() {
    let (_dir, video) = movie_with(&[("Movie.en.srt", ENGLISH_SRT), ("Movie.ru.srt", RUSSIAN_SRT)]);
    let mut owner = owner_with_config(&config_with_languages(true, "ru"));
    begin_and_wait(&mut owner, instance(1), &video);
    let view = owner.menu_view();
    let english = track_named(&view, "Movie.en.srt");
    let russian = track_named(&view, "Movie.ru.srt");
    assert_eq!(selected_file_name(&view).as_deref(), Some("Movie.ru.srt"));

    // Сейчас выбрана последняя дорожка: следующий шаг — «выкл».
    assert_eq!(owner.select_next_track(), CycleOutcome::Off);
    assert_eq!(owner.menu_view().visibility, SubtitleVisibility::Hidden);
    assert_eq!(owner.select_next_track(), CycleOutcome::Track(english));
    assert_eq!(owner.select_next_track(), CycleOutcome::Track(russian));
    assert_eq!(owner.select_next_track(), CycleOutcome::Off);

    // Без дорожек цикл ничего не делает.
    let mut empty = super::owner();
    empty.begin_media(instance(2), SubtitleMediaSource::NotLocal);
    assert_eq!(empty.select_next_track(), CycleOutcome::NoTracks);
}

#[test]
fn sync_nudges_move_a_cue_in_steps_of_one_hundred_milliseconds() {
    let (_dir, video) = movie_with(&[("Movie.srt", LATE_CUE_SRT)]);
    let mut owner = owner();
    let media = instance(1);
    begin_and_wait(&mut owner, media, &video);
    assert!(
        overlay_present(&mut owner, media, 10_000),
        "unshifted start"
    );

    for _ in 0..3 {
        owner.nudge_sync(SyncNudge::Later);
    }
    assert_eq!(
        owner.menu_view().sync_offset,
        SubtitleSyncOffset::from_millis(300)
    );
    assert!(
        !overlay_present(&mut owner, media, 10_250),
        "+0.3 s: not yet at 10.25 s"
    );
    assert!(
        overlay_present(&mut owner, media, 10_300),
        "+0.3 s: present at 10.30 s"
    );

    owner.reset_sync();
    assert_eq!(owner.menu_view().sync_offset, SubtitleSyncOffset::ZERO);
    assert!(
        overlay_present(&mut owner, media, 10_000),
        "reset restores the timing"
    );
    assert!(!overlay_present(&mut owner, media, 9_950));

    for _ in 0..3 {
        owner.nudge_sync(SyncNudge::Earlier);
    }
    assert!(
        overlay_present(&mut owner, media, 9_700),
        "-0.3 s: present at 9.70 s"
    );
    assert!(
        !overlay_present(&mut owner, media, 9_650),
        "-0.3 s: absent at 9.65 s"
    );
}

#[test]
fn nudge_without_media_changes_nothing_and_returns_zero() {
    let mut owner = owner();
    assert_eq!(owner.nudge_sync(SyncNudge::Later), SubtitleSyncOffset::ZERO);
    owner.begin_media(instance(1), SubtitleMediaSource::NotLocal);
    assert_eq!(owner.menu_view().sync_offset, SubtitleSyncOffset::ZERO);
}

#[test]
fn dropped_file_becomes_a_selected_enabled_track_of_the_current_media() {
    let (_dir, video) = movie_with(&[]);
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let dropped = drop_dir.path().join("Subs.ru.srt");
    std::fs::write(&dropped, RUSSIAN_SRT).expect("write dropped file");
    let mut owner = owner_with_config(&SubtitlesConfig::default());
    let media = instance(1);
    begin_and_wait(&mut owner, media, &video);
    assert!(!owner.menu_view().has_tracks, "nothing next to the video");

    let outcome = owner.attach_file(&dropped);
    let AttachOutcome::Attached(track_id) = outcome else {
        panic!("expected Attached, got {outcome:?}");
    };
    settle(&mut owner);

    let view = owner.menu_view();
    assert_eq!(view.visibility, SubtitleVisibility::Shown);
    assert!(
        view.tracks
            .iter()
            .any(|track| track.id == track_id && track.is_selected)
    );
    assert_eq!(
        view.tracks[0].label.origin,
        super::super::menu_view::SubtitleFileOrigin::ChosenByUser
    );
    assert!(overlay_present(&mut owner, media, 2_500));
    assert_eq!(
        owner.take_enabled_preference_change(),
        Some(SubtitleVisibility::Shown),
        "dropping a file is a user decision to see subtitles"
    );

    // Тот же файл повторно не создаёт вторую дорожку.
    assert_eq!(
        owner.attach_file(&dropped),
        AttachOutcome::Attached(track_id)
    );
    assert_eq!(owner.menu_view().tracks.len(), 1);
}

#[test]
fn attach_works_for_non_local_media_too() {
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let dropped = drop_dir.path().join("web.srt");
    std::fs::write(&dropped, RUSSIAN_SRT).expect("write dropped file");
    let mut owner = owner();
    let media = instance(1);
    owner.begin_media(media, SubtitleMediaSource::NotLocal);

    assert!(matches!(
        owner.attach_file(&dropped),
        AttachOutcome::Attached(_)
    ));
    settle(&mut owner);
    assert!(overlay_present(&mut owner, media, 2_500));
}

#[test]
fn attach_without_media_or_with_a_non_subtitle_file_changes_no_state() {
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let subtitle = drop_dir.path().join("a.srt");
    std::fs::write(&subtitle, RUSSIAN_SRT).expect("write");
    let mut owner = owner();

    assert_eq!(
        owner.attach_file(&subtitle),
        AttachOutcome::NoMediaToAttachTo
    );
    assert!(!owner.menu_view().has_tracks);
    assert_eq!(owner.take_enabled_preference_change(), None);

    owner.begin_media(instance(1), SubtitleMediaSource::NotLocal);
    let before = owner.menu_view();
    assert_eq!(
        owner.attach_file(&drop_dir.path().join("notes.txt")),
        AttachOutcome::NotASubtitleFile
    );
    assert_eq!(owner.menu_view(), before);
}

#[test]
fn attached_ass_is_listed_with_a_reason_but_neither_selected_nor_shown() {
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let ass = drop_dir.path().join("styled.ass");
    std::fs::write(&ass, "[Script Info]\n").expect("write");
    let mut owner = owner_with_config(&SubtitlesConfig::default());
    owner.begin_media(instance(1), SubtitleMediaSource::NotLocal);

    let outcome = owner.attach_file(&ass);
    assert!(matches!(outcome, AttachOutcome::AddedButUnavailable(_)));
    let view = owner.menu_view();
    assert_eq!(
        view.visibility,
        SubtitleVisibility::Hidden,
        "visibility untouched"
    );
    assert!(view.tracks.iter().all(|track| !track.is_selected));
    assert_eq!(
        view.tracks[0].availability,
        SubtitleTrackAvailability::Unavailable(SubtitleUnavailableReason::FormatNotSupportedYet)
    );
    assert_eq!(owner.take_enabled_preference_change(), None);
}

#[test]
fn queued_file_is_applied_by_the_next_media_only() {
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let queued = drop_dir.path().join("queued.ru.srt");
    std::fs::write(&queued, RUSSIAN_SRT).expect("write");
    let (_dir_a, video_a) = movie_with(&[]);
    let (_dir_b, video_b) = movie_with(&[]);
    let mut owner = owner_with_config(&SubtitlesConfig::default());

    assert_eq!(
        owner.queue_file_for_next_media(drop_dir.path().join("notes.txt")),
        QueueOutcome::NotASubtitleFile
    );
    assert_eq!(
        owner.queue_file_for_next_media(queued.clone()),
        QueueOutcome::Queued
    );
    // Сброс прежнего media (релиз перед открытием нового) очередь не трогает.
    owner.clear_media();

    begin_and_wait(&mut owner, instance(1), &video_a);
    settle(&mut owner);
    let first = owner.menu_view();
    assert_eq!(first.tracks.len(), 1, "the queued file became a track");
    assert!(first.tracks[0].is_selected);
    assert_eq!(first.visibility, SubtitleVisibility::Shown);
    assert!(overlay_present(&mut owner, instance(1), 2_500));

    begin_and_wait(&mut owner, instance(2), &video_b);
    assert!(
        owner.menu_view().tracks.is_empty(),
        "the queue was consumed by the first media"
    );
}

#[test]
fn cleared_queue_is_not_applied() {
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let queued = drop_dir.path().join("queued.srt");
    std::fs::write(&queued, RUSSIAN_SRT).expect("write");
    let mut owner = owner();
    owner.queue_file_for_next_media(queued);
    owner.clear_queued_files();
    owner.begin_media(instance(1), SubtitleMediaSource::NotLocal);
    assert!(!owner.menu_view().has_tracks);
}

#[test]
fn new_media_forgets_tracks_selection_and_offset_of_the_old_one() {
    let (_dir, video) = movie_with(&[("Movie.srt", LATE_CUE_SRT)]);
    let mut owner = owner();
    let old_media = instance(1);
    begin_and_wait(&mut owner, old_media, &video);
    owner.nudge_sync(SyncNudge::Later);
    assert!(owner.menu_view().has_tracks);

    owner.begin_media(instance(2), SubtitleMediaSource::NotLocal);
    let view = owner.menu_view();
    assert!(!view.has_tracks, "tracks of the old media are gone");
    assert_eq!(
        view.sync_offset,
        SubtitleSyncOffset::ZERO,
        "offset is reset"
    );
    assert!(view.tracks.iter().all(|track| !track.is_selected));
    assert!(!overlay_present(&mut owner, old_media, 10_500));
    assert!(!overlay_present(&mut owner, instance(2), 10_500));
}

#[test]
fn broken_file_becomes_an_unavailable_track_and_other_tracks_stay_selectable() {
    let (_dir, video) = movie_with(&[
        ("Movie.ru.vtt", "this is not webvtt"),
        ("Movie.en.srt", ENGLISH_SRT),
    ]);
    let mut owner = owner_with_config(&config_with_languages(true, "ru, en"));
    let media = instance(1);
    begin_and_wait(&mut owner, media, &video);

    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Unavailable);
    let view = owner.menu_view();
    let broken = view
        .tracks
        .iter()
        .find(|track| track.label.file_name.as_deref() == Some("Movie.ru.vtt"))
        .expect("broken track is listed");
    let SubtitleTrackAvailability::Unavailable(SubtitleUnavailableReason::LoadFailed(reason)) =
        &broken.availability
    else {
        panic!("broken file must be unavailable with a reason");
    };
    assert!(reason.contains("could not be parsed"), "reason: {reason}");
    assert!(
        !overlay_present(&mut owner, media, 2_000),
        "video is untouched, no subtitle"
    );

    // Недоступную дорожку выбрать нельзя; остальные выбираются.
    assert_eq!(
        owner.select_track(broken.id),
        SelectTrackOutcome::TrackUnavailable
    );
    let english = track_named(&view, "Movie.en.srt");
    assert_eq!(owner.select_track(english), SelectTrackOutcome::Selected);
    settle(&mut owner);
    assert!(overlay_present(&mut owner, media, 2_000));
    // Цикл пропускает недоступную дорожку.
    assert_eq!(owner.select_next_track(), CycleOutcome::Off);
    assert_eq!(owner.select_next_track(), CycleOutcome::Track(english));
}

#[test]
fn changed_text_size_rasterizes_once_on_the_next_frame_and_not_on_the_following_ones() {
    let media = instance(1);
    let (mut owner, _dir) = ready_owner(media);
    assert!(overlay_present(&mut owner, media, 2_000));
    assert_eq!(owner.presentation_stats().rasterizer_calls, 1);

    owner.apply_config(&SubtitlesConfig {
        text_size_percent: 150,
        ..enabled_config()
    });
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        1,
        "nothing at config time"
    );
    assert!(overlay_present(&mut owner, media, 2_000));
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        2,
        "restyled on the next frame"
    );
    assert!(overlay_present(&mut owner, media, 2_100));
    assert!(overlay_present(&mut owner, media, 2_200));
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        2,
        "later frames reuse it"
    );

    // Тот же конфиг ещё раз — ничего не меняет.
    owner.apply_config(&SubtitlesConfig {
        text_size_percent: 150,
        ..enabled_config()
    });
    assert!(overlay_present(&mut owner, media, 2_300));
    assert_eq!(owner.presentation_stats().rasterizer_calls, 2);
}

#[test]
fn config_enabled_and_languages_apply_to_the_next_media_not_the_current_one() {
    let (_dir, video) = movie_with(&[("Movie.en.srt", ENGLISH_SRT), ("Movie.ru.srt", RUSSIAN_SRT)]);
    let mut owner = owner_with_config(&config_with_languages(true, "en"));
    let media = instance(1);
    begin_and_wait(&mut owner, media, &video);
    assert_eq!(
        selected_file_name(&owner.menu_view()).as_deref(),
        Some("Movie.en.srt")
    );

    owner.apply_config(&config_with_languages(false, "ru"));
    assert_eq!(
        owner.menu_view().visibility,
        SubtitleVisibility::Shown,
        "current media keeps its state"
    );
    assert!(overlay_present(&mut owner, media, 2_000));

    begin_and_wait(&mut owner, instance(2), &video);
    assert_eq!(
        owner.menu_view().visibility,
        SubtitleVisibility::Hidden,
        "next media starts off"
    );
    owner.set_visibility(SubtitleVisibility::Shown);
    settle(&mut owner);
    assert_eq!(
        selected_file_name(&owner.menu_view()).as_deref(),
        Some("Movie.ru.srt")
    );
}

#[test]
fn changed_load_limits_apply_to_the_next_read() {
    let (_dir, video) = movie_with(&[("Movie.srt", ENGLISH_SRT)]);
    let mut owner = owner();
    // Лимит в 1 МБ режет файл в 2 МБ; лимит по умолчанию (8 МБ) пропустил бы.
    let big_file = _dir.path().join("Movie.big.srt");
    let filler = "1\n00:00:01,000 --> 00:00:02,000\n".to_owned() + &"x".repeat(2 * 1024 * 1024);
    std::fs::write(&big_file, filler).expect("write big file");
    owner.apply_config(&SubtitlesConfig {
        max_file_size_mb: 1,
        ..enabled_config()
    });
    let media = instance(1);
    begin_and_wait(&mut owner, media, &video);
    let view = owner.menu_view();
    let big = track_named(&view, "Movie.big.srt");
    assert_eq!(owner.select_track(big), SelectTrackOutcome::Selected);
    settle(&mut owner);
    let view = owner.menu_view();
    let big_track = view
        .tracks
        .iter()
        .find(|track| track.id == big)
        .expect("listed");
    assert!(matches!(
        &big_track.availability,
        SubtitleTrackAvailability::Unavailable(SubtitleUnavailableReason::LoadFailed(reason)) if reason.contains("byte limit")
    ));
}

#[test]
fn changed_raster_limits_rebuild_the_rasterizer_for_the_next_media() {
    let media = instance(1);
    let (mut owner, dir) = ready_owner(media);
    assert!(owner.rasterizer_statistics().is_some());

    owner.apply_config(&SubtitlesConfig {
        render_cache_mb: 8,
        ..enabled_config()
    });
    assert!(
        owner.rasterizer_statistics().is_some(),
        "the current media keeps its rasterizer"
    );
    let video = dir.path().join("Movie.mkv");
    owner.begin_media(instance(2), SubtitleMediaSource::LocalFile(video));
    assert!(
        owner.rasterizer_statistics().is_none(),
        "the old rasterizer is dropped and rebuilt by the next read"
    );
    settle(&mut owner);
    assert!(owner.rasterizer_statistics().is_some());
    assert!(overlay_present(&mut owner, instance(2), 2_000));
}
