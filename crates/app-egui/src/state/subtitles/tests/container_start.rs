//! Старт шкалы контейнера: файлы SRT/ASS считают от нуля, а кадры локального MPEG-TS идут по
//! сырому времени контейнера (первый кадр на ~1,4 с). Поправка касается только файловых
//! дорожек локального файла; встроенные, web и потоковые остаются как были.

use super::embedded_support::*;
use super::track_selection::{movie_with, overlay_present, settle};
use super::*;
use crate::state::subtitles::sync_offset::SyncNudge;
use subtitle_core::SubtitleSyncOffset;

/// Старт контейнера записи MPEG-TS из задачи владельца.
const CONTAINER_START: Duration = Duration::from_millis(1_400);
/// Одна реплика [0,5 с; 1,5 с) по часам файла.
const EARLY_CUE_SRT: &str = "1\n00:00:00,500 --> 00:00:01,500\nA\n";

fn loaded_owner(media: MediaInstanceId) -> (SubtitleOwner, tempfile::TempDir) {
    let (directory, video) = movie_with(&[("Movie.srt", EARLY_CUE_SRT)]);
    let mut owner = owner();
    begin_and_wait(&mut owner, media, &video);
    (owner, directory)
}

#[test]
fn sidecar_cue_follows_the_container_start_of_a_local_file() {
    let media = instance(1);
    let (mut owner, _directory) = loaded_owner(media);

    assert!(owner.note_container_start(media, CONTAINER_START));

    // Кадр на сыром 2,0 с - это 0,6 с по часам файла: реплика [0,5; 1,5) идёт.
    assert!(overlay_present(&mut owner, media, 2_000));
    assert!(overlay_present(&mut owner, media, 1_900));
    // Кадры в окне старта и после конца реплики (1,4 + 1,5 = 2,9 с) без неё.
    assert!(!overlay_present(&mut owner, media, 600));
    assert!(!overlay_present(&mut owner, media, 1_000));
    assert!(!overlay_present(&mut owner, media, 2_900));
}

#[test]
fn zero_start_keeps_the_previous_behaviour() {
    let media = instance(1);
    let (mut owner, _directory) = loaded_owner(media);

    assert!(
        !owner.note_container_start(media, Duration::ZERO),
        "нулевой старт ничего не меняет"
    );

    assert!(overlay_present(&mut owner, media, 1_000));
    assert!(!overlay_present(&mut owner, media, 2_000));
}

#[test]
fn start_arriving_after_the_file_was_loaded_corrects_the_shown_cue() {
    let media = instance(1);
    let (mut owner, _directory) = loaded_owner(media);
    assert!(
        overlay_present(&mut owner, media, 1_000),
        "до сообщения о старте реплика на старом месте"
    );

    owner.note_container_start(media, CONTAINER_START);

    assert!(
        !overlay_present(&mut owner, media, 1_000),
        "показанная картинка не залипает"
    );
    assert!(overlay_present(&mut owner, media, 2_000));
}

#[test]
fn start_arriving_before_the_file_finished_loading_is_applied_to_it() {
    let media = instance(1);
    let (_directory, video) = movie_with(&[("Movie.srt", EARLY_CUE_SRT)]);
    let mut owner = owner();
    owner.begin_media(media, SubtitleMediaSource::LocalFile(video));

    owner.note_container_start(media, CONTAINER_START);
    settle(&mut owner);

    assert!(overlay_present(&mut owner, media, 2_000));
    assert!(!overlay_present(&mut owner, media, 1_000));
}

#[test]
fn user_sync_offset_applies_on_top_and_does_not_absorb_the_container_start() {
    let media = instance(1);
    let (mut owner, _directory) = loaded_owner(media);
    owner.note_container_start(media, CONTAINER_START);

    for _ in 0..3 {
        owner.nudge_sync(SyncNudge::Later);
    }

    // Старт контейнера не попал в сдвиг пользователя (Z/X, память, уведомления).
    assert_eq!(
        owner.menu_view().sync_offset,
        SubtitleSyncOffset::from_millis(300)
    );
    // Реплика [0,5; 1,5) + старт 1,4 + сдвиг 0,3 = [2,2; 3,2) на сыром времени.
    assert!(!overlay_present(&mut owner, media, 2_100));
    assert!(overlay_present(&mut owner, media, 2_250));
    assert!(!overlay_present(&mut owner, media, 3_250));
}

#[test]
fn start_of_another_media_is_ignored() {
    let media = instance(1);
    let (mut owner, _directory) = loaded_owner(media);

    assert!(!owner.note_container_start(instance(2), CONTAINER_START));

    assert!(overlay_present(&mut owner, media, 1_000));
}

#[test]
fn embedded_track_of_a_local_file_is_not_shifted() {
    let media = instance(2);
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let (directory, video) = movie_with(&[]);
    let mut owner = owner();
    begin_and_wait(&mut owner, media, &video);
    owner.sync_embedded_tracks(Some(media), &fixture.tracks);
    wait_until(&mut owner, "rasterizer", |owner| {
        owner.presenter.has_rasterizer()
    });
    owner.deliver_feed_messages(fixture.feed_messages(media));

    owner.note_container_start(media, CONTAINER_START);

    // Встроенная реплика [0,5; 1,5) лежит на той же шкале PES, что и видео.
    assert!(visible_texts(&mut owner, media, 1_000) > 0);
    assert_eq!(visible_texts(&mut owner, media, 1_700), 0);
    drop(directory);
}

#[test]
fn file_attached_to_a_non_local_media_is_never_shifted() {
    let media = instance(1);
    let (directory, _video) = movie_with(&[("Extra.srt", EARLY_CUE_SRT)]);
    let mut owner = owner();
    owner.begin_media(media, SubtitleMediaSource::NotLocal);

    assert!(
        !owner.note_container_start(media, CONTAINER_START),
        "web и потоки нормализованы транспортом"
    );
    owner.attach_file(&directory.path().join("Extra.srt"));
    settle(&mut owner);

    assert!(overlay_present(&mut owner, media, 1_000));
    assert!(!overlay_present(&mut owner, media, 2_000));
}

/// Охранный тест проводки: `AppState` без окна не собрать, поэтому закрепляем, что насос
/// снимка плеера передаёт старт контейнера владельцу субтитров.
#[test]
fn player_snapshot_pump_forwards_the_container_start_to_the_owner() {
    let source = include_str!("../app_state_api.rs");
    assert!(source.contains("presentation_start"));
    assert!(source.contains(".note_container_start("));
}
