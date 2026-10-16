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

use media_core::TrackId;
use player_core::TrackSummarySnapshot;
use subtitle_core::{SubtitleSyncOffset, SubtitleTrackId};
use subtitle_sidecar::{SidecarLocation, SidecarSubtitleCandidate};

use super::active_track::ActiveSubtitleTrack;
use super::auto_select::{TrackOrigin, TrackTraits};
use super::embedded_track::EmbeddedTrackDescription;
use super::menu_view::{
    SubtitleFileOrigin, SubtitleTrackAvailability, SubtitleTrackLabel, SubtitleUnavailableReason,
};
use super::sidecar_job::SubtitleLoadFailure;

/// Описание дорожки: то, что известно без чтения содержимого.
///
/// Либо файл (кандидат целиком: из него задача загрузки берёт путь и подсказки языка), либо
/// дорожка контейнера (идентификатор плеера и метки).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TrackDescription {
    /// Файл рядом с видео либо добавленный пользователем.
    Sidecar(SidecarSubtitleCandidate),
    /// Дорожка внутри контейнера видео.
    Embedded(EmbeddedTrackDescription),
}

impl TrackDescription {
    /// Кандидат-файл для задачи загрузки; у встроенной дорожки файла нет.
    pub(super) fn sidecar_candidate(&self) -> Option<&SidecarSubtitleCandidate> {
        match self {
            Self::Sidecar(candidate) => Some(candidate),
            Self::Embedded(_) => None,
        }
    }

    /// Описание встроенной дорожки; у файла его нет.
    pub(super) fn embedded(&self) -> Option<&EmbeddedTrackDescription> {
        match self {
            Self::Embedded(description) => Some(description),
            Self::Sidecar(_) => None,
        }
    }

    /// Файл добавлен пользователем, найден рядом или дорожка встроена.
    fn origin(&self) -> SubtitleFileOrigin {
        match self {
            Self::Sidecar(candidate) => match candidate.location {
                SidecarLocation::ChosenByUser => SubtitleFileOrigin::ChosenByUser,
                SidecarLocation::VideoDirectory
                | SidecarLocation::SubtitleDirectory
                | SidecarLocation::MovieNamedDirectory => SubtitleFileOrigin::NextToVideo,
            },
            Self::Embedded(_) => SubtitleFileOrigin::Embedded,
        }
    }

    /// Части подписи для меню.
    fn label(&self) -> SubtitleTrackLabel {
        match self {
            Self::Embedded(description) => description.label(),
            Self::Sidecar(candidate) => SubtitleTrackLabel {
                language: candidate.language.clone(),
                format: Some(candidate.format.to_subtitle_format()),
                origin: self.origin(),
                is_forced: candidate.disposition.is_forced,
                is_hearing_impaired: candidate.is_hearing_impaired,
                title: candidate.title.clone(),
                file_name: candidate
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            },
        }
    }

    /// Формат можно показать уже сейчас.
    fn is_renderable_now(&self) -> bool {
        match self {
            Self::Sidecar(candidate) => candidate.format.is_renderable_now(),
            Self::Embedded(description) => description.is_renderable_now(),
        }
    }
}

/// Причина недоступности для UI (без внутренних типов и путей).
fn unavailable_reason_view(failure: &SubtitleLoadFailure) -> SubtitleUnavailableReason {
    match failure {
        SubtitleLoadFailure::FormatNotSupportedYet { .. }
        | SubtitleLoadFailure::EmbeddedCodecNotSupportedYet => {
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
        self.description.is_renderable_now()
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

    /// Признаки для автовыбора.
    fn traits(&self) -> TrackTraits<'_> {
        let is_renderable = self.is_renderable_now() && self.is_selectable();
        match &self.description {
            TrackDescription::Sidecar(candidate) => TrackTraits {
                origin: TrackOrigin::Sidecar,
                is_default: false,
                language: candidate.language.as_ref(),
                is_forced: candidate.disposition.is_forced,
                is_hearing_impaired: candidate.is_hearing_impaired,
                is_renderable,
            },
            TrackDescription::Embedded(description) => TrackTraits {
                origin: TrackOrigin::Embedded,
                is_default: description.is_default(),
                language: description.language(),
                is_forced: description.is_forced(),
                is_hearing_impaired: description.is_hearing_impaired(),
                is_renderable,
            },
        }
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
            if !matches!(
                failure,
                SubtitleLoadFailure::FormatNotSupportedYet { .. }
                    | SubtitleLoadFailure::EmbeddedCodecNotSupportedYet
            ))
        {
            entry.load = TrackLoadState::NotLoaded;
        }
    }

    /// Дорожка с таким файлом уже есть (повторный бросок того же файла не плодит дубли).
    pub(super) fn find_by_path(&self, path: &Path) -> Option<SubtitleTrackId> {
        self.entries
            .iter()
            .find(|entry| {
                entry
                    .description
                    .sidecar_candidate()
                    .is_some_and(|candidate| candidate.path == path)
            })
            .map(|entry| entry.id)
    }

    fn allocate_id(&mut self) -> SubtitleTrackId {
        let id = SubtitleTrackId::new(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    /// Состояние новой дорожки: непоказываемый формат сразу недоступен с причиной.
    fn initial_state_of(description: &TrackDescription) -> TrackLoadState {
        match description {
            TrackDescription::Sidecar(candidate) if candidate.format.is_renderable_now() => {
                TrackLoadState::NotLoaded
            }
            TrackDescription::Sidecar(candidate) => {
                TrackLoadState::Unavailable(SubtitleLoadFailure::FormatNotSupportedYet {
                    format: candidate.format,
                })
            }
            // Встроенная дорожка готовится владельцем (`Ready` с пустым таймлайном) либо
            // сразу недоступна: файла, который можно «прочитать позже», у неё нет.
            TrackDescription::Embedded(_) => {
                TrackLoadState::Unavailable(SubtitleLoadFailure::EmbeddedCodecNotSupportedYet)
            }
        }
    }

    fn make_entry(&mut self, candidate: &SidecarSubtitleCandidate) -> TrackEntry {
        let description = TrackDescription::Sidecar(candidate.clone());
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
        self.entries.iter().map(|entry| entry.traits()).collect()
    }

    /// Список встроенных дорожек совпадает со строками снимка плеера (без аллокаций).
    ///
    /// Владелец вызывает его на каждом снимке и перестраивает список только при `false`.
    pub(super) fn embedded_matches_snapshot<'a>(
        &self,
        subtitle_rows: impl IntoIterator<Item = &'a TrackSummarySnapshot>,
    ) -> bool {
        let mut embedded = self
            .entries
            .iter()
            .filter_map(|entry| entry.description.embedded());
        for row in subtitle_rows {
            match embedded.next() {
                Some(description) if description.matches_snapshot(row) => {}
                _ => return false,
            }
        }
        embedded.next().is_none()
    }

    /// Дорожка списка по идентификатору дорожки плеера.
    pub(super) fn find_embedded(&self, player_track_id: TrackId) -> Option<SubtitleTrackId> {
        self.entries
            .iter()
            .find(|entry| {
                entry
                    .description
                    .embedded()
                    .is_some_and(|description| description.player_track_id() == player_track_id)
            })
            .map(|entry| entry.id)
    }

    /// Выдаёт новый идентификатор и добавляет в КОНЕЦ списка встроенную дорожку с заданным
    /// начальным состоянием (владелец решает: пустой таймлайн или «не поддержано»).
    pub(super) fn push_embedded(
        &mut self,
        description: EmbeddedTrackDescription,
        load: TrackLoadState,
    ) -> SubtitleTrackId {
        let id = self.allocate_id();
        self.entries.push(TrackEntry {
            id,
            description: TrackDescription::Embedded(description),
            load,
        });
        id
    }

    /// Обновляет описание встроенной дорожки на месте (метки изменились, кодек прежний).
    pub(super) fn replace_embedded_description(
        &mut self,
        id: SubtitleTrackId,
        description: EmbeddedTrackDescription,
    ) {
        if let Some(entry) = self.get_mut(id) {
            entry.description = TrackDescription::Embedded(description);
        }
    }

    /// Убирает встроенные дорожки, которых больше нет в снимке (смена набора дорожек).
    /// Возвращает убранные идентификаторы.
    pub(super) fn retain_embedded(
        &mut self,
        keep: impl Fn(TrackId) -> bool,
    ) -> Vec<SubtitleTrackId> {
        let mut removed = Vec::new();
        self.entries.retain(|entry| {
            let keep_entry = entry
                .description
                .embedded()
                .is_none_or(|description| keep(description.player_track_id()));
            if !keep_entry {
                removed.push(entry.id);
            }
            keep_entry
        });
        removed
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
