//! Список дорожек субтитров текущего media и состояние загрузки каждой.
//!
//! # Владение и инварианты
//! Список принадлежит `SubtitleOwner` и живёт ровно один media: при смене media создаётся
//! новый список, поэтому `SubtitleTrackId` стабилен только внутри одного media (счётчик
//! начинается заново, старые идентификаторы ничего не значат).
//!
//! Загрузка ленивая: таймлайн читается и разбирается только для той дорожки, которую нужно
//! показать. Остальные остаются `NotLoaded` и не стоят ни чтения диска, ни памяти.
//!
//! Все загруженные дорожки одного media делят один сдвиг синхронизации; его меняет
//! [`SubtitleTrackList::apply_sync_offset`], чтобы значения не расходились.

use std::path::Path;

use subtitle_core::{SubtitleSyncOffset, SubtitleTrackId};
use subtitle_sidecar::{SidecarLocation, SidecarSubtitleCandidate};

use super::active_track::ActiveSubtitleTrack;
use super::auto_select::TrackTraits;
use super::menu_view::{
    SubtitleFileOrigin, SubtitleTrackAvailability, SubtitleTrackLabel, SubtitleUnavailableReason,
};
use super::sidecar_job::SubtitleLoadFailure;

/// Описание дорожки: то, что известно без чтения содержимого файла.
///
/// Хранит кандидата целиком: из него же задача загрузки получает путь и подсказки языка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TrackDescription {
    candidate: SidecarSubtitleCandidate,
}

impl TrackDescription {
    fn from_candidate(candidate: &SidecarSubtitleCandidate) -> Self {
        Self {
            candidate: candidate.clone(),
        }
    }

    /// Кандидат для задачи загрузки.
    pub(super) fn candidate(&self) -> &SidecarSubtitleCandidate {
        &self.candidate
    }

    /// Файл добавлен пользователем или найден рядом.
    fn origin(&self) -> SubtitleFileOrigin {
        match self.candidate.location {
            SidecarLocation::ChosenByUser => SubtitleFileOrigin::ChosenByUser,
            SidecarLocation::VideoDirectory
            | SidecarLocation::SubtitleDirectory
            | SidecarLocation::MovieNamedDirectory => SubtitleFileOrigin::NextToVideo,
        }
    }

    /// Части подписи для меню.
    fn label(&self) -> SubtitleTrackLabel {
        SubtitleTrackLabel {
            language: self.candidate.language.clone(),
            format: self.candidate.format.to_subtitle_format(),
            origin: self.origin(),
            is_forced: self.candidate.disposition.is_forced,
            is_hearing_impaired: self.candidate.is_hearing_impaired,
            title: self.candidate.title.clone(),
            file_name: self
                .candidate
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
        }
    }
}

/// Причина недоступности для UI (без внутренних типов и путей).
fn unavailable_reason_view(failure: &SubtitleLoadFailure) -> SubtitleUnavailableReason {
    match failure {
        SubtitleLoadFailure::FormatNotSupportedYet { .. } => {
            SubtitleUnavailableReason::FormatNotSupportedYet
        }
        other => SubtitleUnavailableReason::LoadFailed(other.to_string()),
    }
}

/// Состояние загрузки таймлайна одной дорожки.
pub(super) enum TrackLoadState {
    /// Файл ещё не читали (дорожка не нужна для показа).
    NotLoaded,
    /// Фоновая задача читает файл.
    Loading,
    /// Таймлайн готов к показу.
    Ready(ActiveSubtitleTrack),
    /// Показать нельзя; причина типизирована.
    Unavailable(SubtitleLoadFailure),
}

/// Дорожка списка.
pub(super) struct TrackEntry {
    pub(super) id: SubtitleTrackId,
    pub(super) description: TrackDescription,
    pub(super) load: TrackLoadState,
}

impl TrackEntry {
    /// Можно ли выбрать дорожку (выбор разрешён и пока идёт загрузка).
    pub(super) fn is_selectable(&self) -> bool {
        !matches!(self.load, TrackLoadState::Unavailable(_))
    }

    /// Формат можно показать уже сейчас.
    fn is_renderable_now(&self) -> bool {
        self.description.candidate.format.is_renderable_now()
    }

    /// Доступность для меню.
    pub(super) fn availability(&self) -> SubtitleTrackAvailability {
        match &self.load {
            TrackLoadState::Unavailable(reason) => {
                SubtitleTrackAvailability::Unavailable(unavailable_reason_view(reason))
            }
            TrackLoadState::NotLoaded | TrackLoadState::Loading | TrackLoadState::Ready(_) => {
                SubtitleTrackAvailability::Available
            }
        }
    }

    /// Подпись для меню.
    pub(super) fn label(&self) -> SubtitleTrackLabel {
        self.description.label()
    }
}

/// Список дорожек одного media.
pub(super) struct SubtitleTrackList {
    entries: Vec<TrackEntry>,
    next_id: u64,
}

impl SubtitleTrackList {
    /// Пустой список нового media.
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 0,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Дорожки в порядке меню.
    pub(super) fn entries(&self) -> &[TrackEntry] {
        &self.entries
    }

    pub(super) fn get(&self, id: SubtitleTrackId) -> Option<&TrackEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub(super) fn get_mut(&mut self, id: SubtitleTrackId) -> Option<&mut TrackEntry> {
        self.entries.iter_mut().find(|entry| entry.id == id)
    }

    /// Id дорожки по номеру в списке.
    pub(super) fn id_at(&self, index: usize) -> Option<SubtitleTrackId> {
        self.entries.get(index).map(|entry| entry.id)
    }

    /// Возвращает нечитаемую дорожку в «не загружена», чтобы повторное добавление исправленного
    /// файла попробовало прочитать его снова. Непоказываемый формат остаётся недоступным.
    pub(super) fn allow_retry_after_failure(&mut self, id: SubtitleTrackId) {
        if let Some(entry) = self.get_mut(id)
            && matches!(&entry.load, TrackLoadState::Unavailable(failure)
                if !matches!(failure, SubtitleLoadFailure::FormatNotSupportedYet { .. }))
        {
            entry.load = TrackLoadState::NotLoaded;
        }
    }

    /// Дорожка с таким файлом уже есть (повторный бросок того же файла не плодит дубли).
    pub(super) fn find_by_path(&self, path: &Path) -> Option<SubtitleTrackId> {
        self.entries
            .iter()
            .find(|entry| entry.description.candidate.path == path)
            .map(|entry| entry.id)
    }

    fn allocate_id(&mut self) -> SubtitleTrackId {
        let id = SubtitleTrackId::new(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    /// Состояние новой дорожки: непоказываемый формат сразу недоступен с причиной.
    fn initial_state_of(description: &TrackDescription) -> TrackLoadState {
        if description.candidate.format.is_renderable_now() {
            TrackLoadState::NotLoaded
        } else {
            TrackLoadState::Unavailable(SubtitleLoadFailure::FormatNotSupportedYet {
                format: description.candidate.format,
            })
        }
    }

    fn make_entry(&mut self, candidate: &SidecarSubtitleCandidate) -> TrackEntry {
        let description = TrackDescription::from_candidate(candidate);
        TrackEntry {
            id: self.allocate_id(),
            load: Self::initial_state_of(&description),
            description,
        }
    }

    /// Добавляет найденные поиском дорожки В НАЧАЛО списка (порядок кандидатов сохраняется;
    /// файлы, добавленные пользователем раньше результата поиска, остаются после них).
    /// Возвращает id в порядке кандидатов.
    pub(super) fn insert_discovered(
        &mut self,
        candidates: &[SidecarSubtitleCandidate],
    ) -> Vec<SubtitleTrackId> {
        let discovered: Vec<TrackEntry> = candidates
            .iter()
            .map(|candidate| self.make_entry(candidate))
            .collect();
        let ids = discovered.iter().map(|entry| entry.id).collect();
        self.entries.splice(0..0, discovered);
        ids
    }

    /// Добавляет файл пользователя в конец списка.
    pub(super) fn push_chosen(&mut self, candidate: &SidecarSubtitleCandidate) -> SubtitleTrackId {
        let entry = self.make_entry(candidate);
        let id = entry.id;
        self.entries.push(entry);
        id
    }

    /// Признаки дорожек для автовыбора (в порядке списка).
    pub(super) fn traits(&self) -> Vec<TrackTraits<'_>> {
        self.entries
            .iter()
            .map(|entry| TrackTraits {
                language: entry.description.candidate.language.as_ref(),
                is_forced: entry.description.candidate.disposition.is_forced,
                is_hearing_impaired: entry.description.candidate.is_hearing_impaired,
                is_renderable: entry.is_renderable_now() && entry.is_selectable(),
            })
            .collect()
    }

    /// Применяет общий сдвиг ко всем уже загруженным дорожкам.
    pub(super) fn apply_sync_offset(&mut self, offset: SubtitleSyncOffset) {
        for entry in &mut self.entries {
            if let TrackLoadState::Ready(track) = &mut entry.load {
                track.set_sync_offset(offset);
            }
        }
    }
}
