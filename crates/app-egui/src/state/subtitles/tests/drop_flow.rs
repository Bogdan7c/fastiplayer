//! Бросок файлов субтитров от жеста до дорожки: настоящий диспетчер внешнего открытия
//! (классификация, маршрутизация, шаги) и настоящий владелец субтитров. `AppState` без окна
//! не собрать, поэтому хост-заглушка делает то же, что `AppStateExternalOpenHost`: зовёт
//! intent-ы владельца и показывает те же тексты. Открытие видео и очередь плейлиста
//! записываются как обращения, чтобы видеть, что очередь НЕ тронута.

use std::path::PathBuf;

use fastiplayer_config::DroppedPlaylistFileAction;

use super::track_selection::{RUSSIAN_SRT, movie_with, overlay_present, settle};
use super::*;
use crate::external_open::request::{
    DropTarget, DroppedWebUrl, ExternalOpenItem, ExternalOpenRequest,
};
use crate::external_open::{ExternalOpenHost, dispatch_external_open_request};
use crate::playlist_runtime::{DroppedCollectionEntry, PlaylistImportIntent};
use crate::state::dropped_subtitles::OpenActivity;
use crate::state::subtitles::intents::SubtitleVisibility;
use crate::subtitle_message::attach_batch_message;

/// Хост: владелец субтитров + журнал обращений к очереди и уведомлений.
struct DropHost {
    owner: SubtitleOwner,
    /// Обращения к открытию видео и очереди (должны быть пусты, если броска не касались).
    queue_calls: Vec<String>,
    /// Тексты transient/info уведомлений в порядке показа.
    toasts: Vec<String>,
}

impl DropHost {
    fn new() -> Self {
        Self {
            owner: owner(),
            queue_calls: Vec::new(),
            toasts: Vec::new(),
        }
    }
}

impl ExternalOpenHost for DropHost {
    fn open_in_progress(&self) -> bool {
        false
    }
    fn notify_open_still_in_progress(&mut self) {
        self.toasts.push("busy".to_owned());
    }
    fn notify_info(&mut self, message: String) {
        self.toasts.push(message);
    }
    fn open_single_media_file_like_open_button(&mut self, path: PathBuf) {
        self.queue_calls.push(format!("open:{}", path.display()));
    }
    fn replace_queue_with_media_files(&mut self, paths: Vec<PathBuf>) {
        self.queue_calls.push(format!("replace:{}", paths.len()));
    }
    fn append_media_files_to_queue(&mut self, paths: Vec<PathBuf>) {
        self.queue_calls.push(format!("append:{}", paths.len()));
    }
    fn start_dropped_collection_walk(&mut self, _: Vec<DroppedCollectionEntry>, _: DropTarget) {
        self.queue_calls.push("walk".to_owned());
    }
    fn dropped_playlist_file_action(&self) -> DroppedPlaylistFileAction {
        DroppedPlaylistFileAction::ByDropTarget
    }
    fn import_playlist_file(&mut self, _: PathBuf, _: PlaylistImportIntent) {
        self.queue_calls.push("import".to_owned());
    }
    fn add_web_url_like_add_url_button(&mut self, _: DroppedWebUrl) {
        self.queue_calls.push("add-url".to_owned());
    }
    fn replace_queue_with_web_url(&mut self, _: DroppedWebUrl) {
        self.queue_calls.push("replace-url".to_owned());
    }
    fn attach_subtitle_files_to_current_media(&mut self, paths: Vec<PathBuf>) {
        let outcomes = self.owner.attach_files(&paths);
        if let Some(message) = attach_batch_message(&outcomes) {
            self.toasts.push(message.to_owned());
        }
    }
    fn queue_subtitle_files_for_opened_media(&mut self, paths: Vec<PathBuf>) {
        self.owner.queue_files_for_next_media(paths);
    }
}

fn drop_onto(target: DropTarget, host: &mut DropHost, paths: &[&PathBuf]) {
    let items = paths
        .iter()
        .map(|path| ExternalOpenItem::LocalPath((*path).clone()))
        .collect();
    dispatch_external_open_request(host, ExternalOpenRequest { target, items });
}

/// Файл с содержимым во временном каталоге.
fn file_in(directory: &tempfile::TempDir, name: &str, content: &str) -> PathBuf {
    let path = directory.path().join(name);
    std::fs::write(&path, content).expect("write dropped file");
    path
}

#[test]
fn subtitle_dropped_on_playing_video_is_shown_and_the_queue_is_untouched() {
    let (_dir, video) = movie_with(&[]);
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let subtitle = file_in(&drop_dir, "Extra.SRT", RUSSIAN_SRT);
    let mut host = DropHost::new();
    begin_and_wait(&mut host.owner, instance(1), &video);

    drop_onto(DropTarget::Video, &mut host, &[&subtitle]);
    settle(&mut host.owner);

    let view = host.owner.menu_view();
    assert_eq!(view.tracks.len(), 1);
    assert!(view.tracks[0].is_selected);
    assert_eq!(view.visibility, SubtitleVisibility::Shown);
    assert!(overlay_present(&mut host.owner, instance(1), 2_500));
    assert!(
        host.queue_calls.is_empty(),
        "queue calls: {:?}",
        host.queue_calls
    );
    assert!(host.toasts.is_empty(), "toasts: {:?}", host.toasts);
}

#[test]
fn several_subtitles_dropped_alone_are_all_attached_and_the_last_is_selected() {
    let (_dir, video) = movie_with(&[]);
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let first = file_in(&drop_dir, "one.srt", RUSSIAN_SRT);
    let second = file_in(
        &drop_dir,
        "two.vtt",
        "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nHi\n",
    );
    let mut host = DropHost::new();
    begin_and_wait(&mut host.owner, instance(1), &video);

    drop_onto(DropTarget::Playlist, &mut host, &[&first, &second]);
    settle(&mut host.owner);

    let view = host.owner.menu_view();
    assert_eq!(view.tracks.len(), 2, "both files are attached");
    assert!(!view.tracks[0].is_selected);
    assert!(
        view.tracks[1].is_selected,
        "the last dropped file is selected"
    );
    assert!(host.queue_calls.is_empty());
}

#[test]
fn subtitle_dropped_without_media_shows_the_hint_and_remembers_nothing() {
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let subtitle = file_in(&drop_dir, "Lonely.srt", RUSSIAN_SRT);
    let mut host = DropHost::new();

    drop_onto(DropTarget::Video, &mut host, &[&subtitle]);

    assert_eq!(host.toasts, vec!["Open a video first, then add subtitles."]);
    assert!(host.queue_calls.is_empty());
    // Ничего не запомнено: следующее видео остаётся без дорожек.
    host.owner
        .begin_media(instance(1), SubtitleMediaSource::NotLocal);
    assert!(!host.owner.menu_view().has_tracks);
}

#[test]
fn ass_dropped_on_playing_video_is_listed_as_unavailable_with_a_toast() {
    let (_dir, video) = movie_with(&[]);
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let ass = file_in(&drop_dir, "Styled.ass", "[Script Info]\nTitle: x\n");
    let mut host = DropHost::new();
    begin_and_wait(&mut host.owner, instance(1), &video);

    drop_onto(DropTarget::Video, &mut host, &[&ass]);

    assert_eq!(
        host.toasts,
        vec!["This subtitle format is not supported yet."]
    );
    let view = host.owner.menu_view();
    assert_eq!(view.tracks.len(), 1);
    assert!(
        !view.tracks[0].is_selected,
        "an unavailable track is not selected"
    );
    assert!(host.queue_calls.is_empty());
}

#[test]
fn video_with_subtitle_in_one_drop_attaches_it_once_the_video_is_installed() {
    let (_dir, video) = movie_with(&[]);
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let subtitle = file_in(&drop_dir, "Movie.ru.srt", RUSSIAN_SRT);
    let mut host = DropHost::new();

    drop_onto(DropTarget::Video, &mut host, &[&video, &subtitle]);

    // Видео открывается по обычному пути, субтитры не попали ни в очередь, ни в дорожки.
    assert_eq!(host.queue_calls, vec![format!("open:{}", video.display())]);
    assert!(!host.owner.menu_view().has_tracks);
    // Установка видео: файл становится выбранной дорожкой.
    begin_and_wait(&mut host.owner, instance(1), &video);
    settle(&mut host.owner);
    let view = host.owner.menu_view();
    assert_eq!(view.tracks.len(), 1);
    assert!(view.tracks[0].is_selected);
    assert_eq!(view.visibility, SubtitleVisibility::Shown);
    assert!(overlay_present(&mut host.owner, instance(1), 2_500));
}

#[test]
fn subtitle_does_not_stick_to_an_unrelated_video_when_the_open_fails() {
    let (_dir, video) = movie_with(&[]);
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let subtitle = file_in(&drop_dir, "Movie.ru.srt", RUSSIAN_SRT);
    let mut host = DropHost::new();

    drop_onto(DropTarget::Video, &mut host, &[&video, &subtitle]);
    // Открытие провалилось: ни одна работа по открытию не идёт, видео не установлено.
    // То же условие проверяет `AppState::settle_dropped_subtitles`.
    assert!(OpenActivity::default().is_idle());
    host.owner.clear_queued_files();

    // Позже пользователь открывает другое видео: чужие субтитры к нему не липнут.
    let (_other_dir, other_video) = movie_with(&[]);
    begin_and_wait(&mut host.owner, instance(2), &other_video);
    assert!(!host.owner.menu_view().has_tracks);
}

#[test]
fn playlist_panel_mixed_drop_tells_to_drop_onto_a_playing_video_and_attaches_nothing() {
    let (_dir, video) = movie_with(&[]);
    let drop_dir = tempfile::tempdir().expect("tempdir");
    let subtitle = file_in(&drop_dir, "Movie.ru.srt", RUSSIAN_SRT);
    let mut host = DropHost::new();

    drop_onto(DropTarget::Playlist, &mut host, &[&video, &subtitle]);

    assert_eq!(host.queue_calls, vec!["append:1"]);
    assert_eq!(host.toasts, vec!["Drop subtitles onto a playing video."]);
    host.owner
        .begin_media(instance(1), SubtitleMediaSource::NotLocal);
    assert!(!host.owner.menu_view().has_tracks, "nothing was remembered");
}
