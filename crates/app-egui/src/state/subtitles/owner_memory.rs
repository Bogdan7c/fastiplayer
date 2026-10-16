//! Связь владельца субтитров с памятью выбора «для каждого видео».
//!
//! # Границы
//! - Сама память (хранилище, ключ видео, запись на диск) живёт в `state/track_memory`. Владелец
//!   субтитров о ней НЕ знает: он принимает готовый [`SubtitleRestoreRequest`] при старте media
//!   и отдаёт [`SubtitleMemoryChanges`] — только то, что изменил пользователь.
//! - Приоритет (решение владельца): память видео > автовыбор по языкам > флаги файла.
//!
//! # Восстановление
//! Дорожки приходят в разное время: файлы рядом — из фонового поиска, встроенные — из снимка
//! плеера, причём в любом порядке. Поэтому запомненный выбор хранится как «ожидающий»
//! ([`PendingRememberedChoice`]) и сверяется со списком дорожек каждый раз, когда запускается
//! автовыбор (`reselect_automatically`). Нашлась дорожка — выбрана, как если бы её выбрал
//! пользователь (автовыбор её больше не перебивает), но глобальная настройка «субтитры
//! включены» НЕ меняется и в память этот выбор повторно не пишется. Не нашлась (файл удалён,
//! дорожки в контейнере нет) — ожидание снимается без ошибки, работает обычный автовыбор.
//!
//! # Захват
//! Любое действие пользователя (выбор дорожки, включить/выключить, сдвиг, брошенный вместе с
//! видео файл) ставит флаг в [`MemoryDirty`]. Автовыбор и восстановление флаги не ставят,
//! поэтому «автоматическое» в память не попадает.

use std::path::{Path, PathBuf};

use player_core::MediaInstanceId;
use playlist_state::{
    RememberedFilePath, RememberedSubtitleChoice, RememberedTrackHint, TrackHintCandidate,
    TrackHintMatch, find_matching_track,
};
use subtitle_core::SubtitleSyncOffset;

use super::SubtitleOwner;
use super::intents::SubtitleVisibility;
use super::media_state::MediaSubtitles;
use super::track_list::TrackDescription;

/// Что память помнит про субтитры видео, которое только что установлено.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubtitleRestoreRequest {
    /// Выбор пользователя; `None` — для этого видео ничего не запомнено.
    pub(crate) choice: Option<RememberedSubtitleChoice>,
    /// Запомненный сдвиг синхронизации (`ZERO`, если не менялся).
    pub(crate) sync_offset: SubtitleSyncOffset,
}

impl SubtitleRestoreRequest {
    /// Память пуста: работает обычный автовыбор, сдвиг нулевой.
    #[cfg(test)]
    pub(crate) const fn nothing_remembered() -> Self {
        Self {
            choice: None,
            sync_offset: SubtitleSyncOffset::ZERO,
        }
    }
}

/// Выбор из памяти, который ещё ищет свою дорожку.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PendingRememberedChoice {
    /// Встроенная дорожка: ищется по описанию среди встроенных дорожек.
    Embedded(RememberedTrackHint),
    /// Файл субтитров: ищется среди найденных рядом, иначе подключается, если ещё существует.
    File(PathBuf),
}

impl PendingRememberedChoice {
    /// Ожидается ли именно этот файл.
    pub(super) fn is_file(&self, path: &Path) -> bool {
        matches!(self, Self::File(expected) if expected == path)
    }

    /// Файл, существование которого должна проверить фоновая задача.
    pub(super) fn file_to_check(&self) -> Option<PathBuf> {
        match self {
            Self::File(path) => Some(path.clone()),
            Self::Embedded(_) => None,
        }
    }
}

/// Что изменил пользователь с последней выдачи в память.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct MemoryDirty {
    /// Менялся выбор дорожки или «включено/выключено».
    pub(super) choice: bool,
    /// Менялся сдвиг синхронизации.
    pub(super) sync_offset: bool,
}

impl MemoryDirty {
    fn is_clean(self) -> bool {
        !self.choice && !self.sync_offset
    }
}

/// Изменения, сделанные пользователем в субтитрах одного media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubtitleMemoryChanges {
    /// Media, к которому относятся изменения: память отбросит чужие (устаревшие).
    pub(crate) media_instance_id: MediaInstanceId,
    /// Новый выбор; `None` — выбор не менялся либо записывать нечего.
    pub(crate) choice: Option<RememberedSubtitleChoice>,
    /// Новый сдвиг; `None` — не менялся.
    pub(crate) sync_offset: Option<SubtitleSyncOffset>,
}

impl MediaSubtitles {
    /// Пользователь совершил действие с выбором субтитров: ожидание запомненного выбора
    /// больше не нужно (решает сам пользователь), а новый выбор надо отдать в память.
    pub(super) fn note_user_choice(&mut self) {
        self.pending_memory = None;
        self.memory_dirty.choice = true;
    }
}

/// Применяет запомненное к только что созданному состоянию media (до первых дорожек).
///
/// Сдвиг действует всегда. Выбор — только если пользователь не принёс файл вместе с видео
/// (бросок «видео + субтитры» важнее памяти).
pub(super) fn apply_restore_request(media: &mut MediaSubtitles, request: SubtitleRestoreRequest) {
    media.sync_offset = request.sync_offset;
    if media.user_chose_track {
        return;
    }
    match request.choice {
        None => {}
        // «Выключено»: только видимость этого media; глобальное предпочтение не трогаем.
        Some(RememberedSubtitleChoice::Off) => media.visibility = SubtitleVisibility::Hidden,
        Some(RememberedSubtitleChoice::Embedded(hint)) => {
            media.pending_memory = Some(PendingRememberedChoice::Embedded(hint));
        }
        Some(RememberedSubtitleChoice::File(path)) => {
            media.pending_memory =
                Some(PendingRememberedChoice::File(path.as_path().to_path_buf()));
        }
    }
}

/// Исход сверки ожидающего выбора со списком дорожек.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingResolution {
    /// Ждать было нечего.
    NothingPending,
    /// Подходящих дорожек ещё нет: ждём следующего обновления списка.
    Waiting,
    /// Дорожка найдена и выбрана.
    Selected,
    /// Дорожки нет или её нельзя выбрать: ожидание снято, работает автовыбор.
    GaveUp,
}

/// Сверяет ожидающий выбор из памяти со списком дорожек и, если дорожка найдена, выбирает её.
pub(super) fn resolve_pending_remembered_choice(media: &mut MediaSubtitles) -> PendingResolution {
    let Some(pending) = media.pending_memory.clone() else {
        return PendingResolution::NothingPending;
    };
    let found = match &pending {
        PendingRememberedChoice::Embedded(hint) => find_embedded_track(media, hint),
        PendingRememberedChoice::File(path) => find_file_track(media, path),
    };
    match found {
        TrackLookup::Wait => PendingResolution::Waiting,
        TrackLookup::Missing => {
            media.pending_memory = None;
            tracing::debug!("The remembered subtitle track is gone; automatic selection is used");
            PendingResolution::GaveUp
        }
        TrackLookup::Found(track_id) => {
            media.pending_memory = None;
            let selectable = media
                .tracks
                .get(track_id)
                .is_some_and(|entry| entry.is_selectable());
            if !selectable {
                tracing::debug!(
                    "The remembered subtitle track cannot be shown; automatic selection is used"
                );
                return PendingResolution::GaveUp;
            }
            media.selected = Some(track_id);
            // Как выбор пользователя для автовыбора (не перебивать), но без пометки для памяти.
            media.user_chose_track = true;
            media.visibility = SubtitleVisibility::Shown;
            PendingResolution::Selected
        }
    }
}

/// Результат поиска дорожки по запомненному описанию.
enum TrackLookup {
    /// Нашлась.
    Found(subtitle_core::SubtitleTrackId),
    /// Список ещё может пополниться: решать рано.
    Wait,
    /// Дорожки нет, и она не появится.
    Missing,
}

/// Встроенные дорожки в порядке снимка плеера вместе с описанием для сверки.
fn embedded_candidates(
    media: &MediaSubtitles,
) -> Vec<(subtitle_core::SubtitleTrackId, TrackHintCandidate)> {
    media
        .tracks
        .entries()
        .iter()
        .filter_map(|entry| Some((entry.id, entry.description.embedded()?)))
        .enumerate()
        .map(|(ordinal, (track_id, description))| {
            let ordinal = u32::try_from(ordinal).unwrap_or(u32::MAX);
            (track_id, description.track_hint_candidate(ordinal))
        })
        .collect()
}

fn find_embedded_track(media: &MediaSubtitles, hint: &RememberedTrackHint) -> TrackLookup {
    let candidates = embedded_candidates(media);
    if candidates.is_empty() {
        // Встроенные дорожки приходят из снимка плеера и могут появиться позже поиска файлов.
        return TrackLookup::Wait;
    }
    let descriptions: Vec<TrackHintCandidate> = candidates
        .iter()
        .map(|(_, candidate)| candidate.clone())
        .collect();
    match find_matching_track(hint, &descriptions) {
        TrackHintMatch::Exact(position) | TrackHintMatch::Approximate(position) => candidates
            .get(position)
            .map_or(TrackLookup::Missing, |(track_id, _)| {
                TrackLookup::Found(*track_id)
            }),
        TrackHintMatch::NotFound => TrackLookup::Missing,
    }
}

fn find_file_track(media: &MediaSubtitles, path: &Path) -> TrackLookup {
    if let Some(track_id) = media.tracks.find_by_path(path) {
        return TrackLookup::Found(track_id);
    }
    match media.discovery {
        // Поиск (он же проверяет существование запомненного файла) ещё идёт.
        super::media_state::DiscoveryState::Searching(_) => TrackLookup::Wait,
        super::media_state::DiscoveryState::Finished
        | super::media_state::DiscoveryState::Failed(_) => TrackLookup::Missing,
    }
}

/// Выбор пользователя в виде записи памяти; `None` — записывать нечего.
fn choice_for_memory(media: &MediaSubtitles) -> Option<RememberedSubtitleChoice> {
    if media.visibility == SubtitleVisibility::Hidden {
        return Some(RememberedSubtitleChoice::Off);
    }
    let entry = media.tracks.get(media.selected?)?;
    match &entry.description {
        TrackDescription::Embedded(description) => {
            let ordinal = embedded_candidates(media)
                .iter()
                .position(|(track_id, _)| *track_id == entry.id)?;
            let candidate =
                description.track_hint_candidate(u32::try_from(ordinal).unwrap_or(u32::MAX));
            match RememberedTrackHint::new(
                candidate.language,
                candidate.title,
                candidate.codec_id,
                candidate.ordinal_among_kind,
            ) {
                Ok(hint) => Some(RememberedSubtitleChoice::Embedded(hint)),
                Err(error) => {
                    tracing::debug!(%error, "The embedded subtitle track cannot be remembered");
                    None
                }
            }
        }
        TrackDescription::Sidecar(candidate) => {
            match RememberedFilePath::new(candidate.path.clone()) {
                Ok(path) => Some(RememberedSubtitleChoice::File(path)),
                Err(error) => {
                    // Путь в лог не пишем: достаточно причины.
                    tracing::debug!(%error, "The subtitle file path cannot be remembered");
                    None
                }
            }
        }
    }
}

impl SubtitleOwner {
    /// Изменения пользователя с прошлой выдачи (выдаются один раз).
    ///
    /// Автовыбор и восстановление из памяти сюда не попадают: флаги ставят только действия
    /// пользователя (`note_user_choice`, сдвиг).
    pub(crate) fn take_memory_changes(&mut self) -> Option<SubtitleMemoryChanges> {
        let media = self.media.as_mut()?;
        if media.memory_dirty.is_clean() {
            return None;
        }
        let dirty = std::mem::take(&mut media.memory_dirty);
        let choice = if dirty.choice {
            choice_for_memory(media)
        } else {
            None
        };
        let sync_offset = dirty.sync_offset.then_some(media.sync_offset);
        if choice.is_none() && sync_offset.is_none() {
            return None;
        }
        Some(SubtitleMemoryChanges {
            media_instance_id: media.media_instance_id,
            choice,
            sync_offset,
        })
    }
}
