//! Intent-API владельца субтитров: то, что вызывают кнопка CC, меню, хоткеи и внешнее
//! открытие. Каждый метод описывает намерение пользователя и сам решает, что менять.
//!
//! Общие правила:
//! - любое изменение выбора, видимости или сдвига сбрасывает показанный overlay, чтобы
//!   следующий кадр был нарисован заново (обычные кадры по-прежнему не растеризуются);
//! - «включено» запоминается (`default_visibility`) и наружу отдаётся через
//!   [`SubtitleOwner::take_enabled_preference_change`] ТОЛЬКО когда это сделал пользователь,
//!   а не автовыбор: так сохранение в конфиг не перетирается автоматикой;
//! - недоступные дорожки (ASS до сессии 08, нечитаемый файл) выбрать нельзя, и это
//!   сообщается типизированным ответом, а не молчаливым игнорированием.

use std::path::{Path, PathBuf};

use subtitle_core::{SubtitleSyncOffset, SubtitleTrackId};
use subtitle_sidecar::{SidecarSubtitleCandidate, describe_subtitle_file};

use super::SubtitleOwner;
use super::auto_select::auto_select;
use super::intents::{
    AttachOutcome, CycleOutcome, QueueOutcome, SelectTrackOutcome, SubtitleVisibility,
};
use super::media_state::MediaSubtitles;
use super::menu_view::{SubtitleMenuTrack, SubtitleMenuView};
use super::sync_offset::{SyncNudge, nudged_offset};

impl SubtitleOwner {
    /// Включает или выключает субтитры по воле пользователя.
    pub(super) fn set_visibility(&mut self, visibility: SubtitleVisibility) {
        let previous = self
            .media
            .as_ref()
            .map_or(self.default_visibility, |media| media.visibility);
        if previous != visibility {
            self.enabled_preference_change = Some(visibility);
        }
        self.default_visibility = visibility;
        if let Some(media) = self.media.as_mut() {
            media.visibility = visibility;
            media.note_user_choice();
        }
        if visibility == SubtitleVisibility::Shown {
            self.select_automatically_if_nothing_selected();
        }
        self.presenter.forget_shown_overlay();
        self.ensure_display_track_loading();
    }

    /// Переключает видимость и возвращает новое состояние.
    pub(super) fn toggle_visibility(&mut self) -> SubtitleVisibility {
        let current = self
            .media
            .as_ref()
            .map_or(self.default_visibility, |media| media.visibility);
        let next = current.toggled();
        self.set_visibility(next);
        next
    }

    /// Если выбранной дорожки нет, выбирает её автовыбором (правила решения 14).
    fn select_automatically_if_nothing_selected(&mut self) {
        let preferred = self.preferred_languages();
        let Some(media) = self.media.as_mut() else {
            return;
        };
        if media.selected.is_some() || media.user_chose_track {
            return;
        }
        let traits = media.tracks.traits();
        let index = auto_select(&traits, &preferred).for_enabled;
        drop(traits);
        media.selected = index.and_then(|index| media.tracks.id_at(index));
    }

    /// Выбирает дорожку и включает субтитры.
    pub(super) fn select_track(&mut self, track_id: SubtitleTrackId) -> SelectTrackOutcome {
        let Some(media) = self.media.as_mut() else {
            return SelectTrackOutcome::UnknownTrack;
        };
        let Some(entry) = media.tracks.get(track_id) else {
            return SelectTrackOutcome::UnknownTrack;
        };
        if !entry.is_selectable() {
            return SelectTrackOutcome::TrackUnavailable;
        }
        media.selected = Some(track_id);
        media.user_chose_track = true;
        media.note_user_choice();
        let was_hidden = media.visibility != SubtitleVisibility::Shown;
        media.visibility = SubtitleVisibility::Shown;
        self.default_visibility = SubtitleVisibility::Shown;
        if was_hidden {
            self.enabled_preference_change = Some(SubtitleVisibility::Shown);
        }
        self.presenter.forget_shown_overlay();
        self.ensure_display_track_loading();
        SelectTrackOutcome::Selected
    }

    /// Следующая дорожка по кругу: дорожки в порядке меню, затем «выкл», затем первая.
    /// Недоступные дорожки пропускаются.
    pub(super) fn select_next_track(&mut self) -> CycleOutcome {
        let Some(media) = &self.media else {
            return CycleOutcome::NoTracks;
        };
        let selectable: Vec<SubtitleTrackId> = media
            .tracks
            .entries()
            .iter()
            .filter(|entry| entry.is_selectable())
            .map(|entry| entry.id)
            .collect();
        if selectable.is_empty() {
            return CycleOutcome::NoTracks;
        }
        let current_position = match (media.visibility, media.selected) {
            (SubtitleVisibility::Shown, Some(selected)) => {
                selectable.iter().position(|id| *id == selected)
            }
            _ => None,
        };
        let next_track = match current_position {
            None => selectable.first(),
            Some(position) => selectable.get(position + 1),
        };
        match next_track {
            Some(&track_id) => {
                // Дорожка только что выбрана из доступных: отказа быть не может.
                self.select_track(track_id);
                CycleOutcome::Track(track_id)
            }
            None => {
                self.set_visibility(SubtitleVisibility::Hidden);
                CycleOutcome::Off
            }
        }
    }

    /// Сдвигает синхронизацию на шаг и возвращает новый сдвиг. Без media — `ZERO` и без
    /// изменений.
    pub(super) fn nudge_sync(&mut self, nudge: SyncNudge) -> SubtitleSyncOffset {
        let Some(media) = self.media.as_mut() else {
            return SubtitleSyncOffset::ZERO;
        };
        let next = nudged_offset(media.sync_offset, nudge);
        self.apply_sync_offset(next);
        next
    }

    /// Сбрасывает сдвиг в ноль.
    pub(super) fn reset_sync(&mut self) {
        self.apply_sync_offset(SubtitleSyncOffset::ZERO);
    }

    fn apply_sync_offset(&mut self, offset: SubtitleSyncOffset) {
        let Some(media) = self.media.as_mut() else {
            return;
        };
        media.sync_offset = offset;
        media.memory_dirty.sync_offset = true;
        media.tracks.apply_sync_offset(offset);
        self.presenter.forget_shown_overlay();
    }

    /// Добавляет файл субтитров к ТЕКУЩЕМУ media (любой источник), выбирает его и включает.
    pub(super) fn attach_file(&mut self, path: &Path) -> AttachOutcome {
        let Some(candidate) = describe_subtitle_file(path) else {
            return AttachOutcome::NotASubtitleFile;
        };
        let Some(media) = self.media.as_mut() else {
            return AttachOutcome::NoMediaToAttachTo;
        };
        // Тот же файл повторно не плодит дорожку: выбираем существующую.
        let track_id = match media.tracks.find_by_path(path) {
            Some(existing) => {
                media.tracks.allow_retry_after_failure(existing);
                existing
            }
            None => media.tracks.push_chosen(&candidate),
        };
        match self.select_track(track_id) {
            SelectTrackOutcome::Selected => AttachOutcome::Attached(track_id),
            SelectTrackOutcome::TrackUnavailable => {
                // Файл в списке с причиной (например, ASS), но показать его нельзя.
                AttachOutcome::AddedButUnavailable(track_id)
            }
            SelectTrackOutcome::UnknownTrack => AttachOutcome::NoMediaToAttachTo,
        }
    }

    /// Добавляет несколько файлов к текущему media по очереди: последний принятый становится
    /// выбранным. Исход каждого файла отдаётся отдельно (в порядке `paths`).
    pub(super) fn attach_files(&mut self, paths: &[PathBuf]) -> Vec<AttachOutcome> {
        paths.iter().map(|path| self.attach_file(path)).collect()
    }

    /// Запоминает несколько файлов для ближайшего `begin_media`; исходы — в порядке `paths`.
    pub(super) fn queue_files_for_next_media(&mut self, paths: Vec<PathBuf>) -> Vec<QueueOutcome> {
        paths
            .into_iter()
            .map(|path| self.queue_file_for_next_media(path))
            .collect()
    }

    /// Ставит файл в очередь для ближайшего `begin_media` («видео + субтитры» броском).
    pub(super) fn queue_file_for_next_media(&mut self, path: PathBuf) -> QueueOutcome {
        let Some(candidate) = describe_subtitle_file(&path) else {
            return QueueOutcome::NotASubtitleFile;
        };
        let already_queued = self
            .queued_files
            .iter()
            .any(|queued: &SidecarSubtitleCandidate| queued.path == candidate.path);
        if !already_queued {
            self.queued_files.push(candidate);
        }
        QueueOutcome::Queued
    }

    /// Забывает файлы, ждавшие следующего media (открытие видео не состоялось).
    pub(super) fn clear_queued_files(&mut self) {
        self.queued_files.clear();
    }

    /// Изменение «включено» по воле пользователя, ещё не сохранённое в конфиг.
    pub(super) fn take_enabled_preference_change(&mut self) -> Option<SubtitleVisibility> {
        self.enabled_preference_change.take()
    }

    /// Снимок для кнопки CC и меню.
    pub(super) fn menu_view(&self) -> SubtitleMenuView {
        let Some(media) = &self.media else {
            return SubtitleMenuView::empty(self.default_visibility);
        };
        SubtitleMenuView {
            has_tracks: !media.tracks.is_empty(),
            visibility: media.visibility,
            tracks: menu_tracks(media),
            sync_offset: media.sync_offset,
        }
    }
}

/// Строки меню; отмечена выбранная дорожка при включённых субтитрах.
fn menu_tracks(media: &MediaSubtitles) -> Vec<SubtitleMenuTrack> {
    let selection_is_visible = media.visibility == SubtitleVisibility::Shown;
    media
        .tracks
        .entries()
        .iter()
        .map(|entry| SubtitleMenuTrack {
            id: entry.id,
            label: entry.label(),
            is_selected: selection_is_visible && media.selected == Some(entry.id),
            availability: entry.availability(),
        })
        .collect()
}
