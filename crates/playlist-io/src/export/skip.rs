//! Типизированный учёт записей, которые нельзя представить в формате экспорта.
//!
//! Решение владельца (B4): неподходящая запись не валит весь экспорт, а
//! пропускается; UI показывает «экспортировано M, пропущено N». Этот модуль
//! владеет только словарём причин и счётчиками; текст для пользователя
//! формирует UI-слой. Ошибки записи файла сюда не относятся.

use super::{PlaylistExportIneligible, PlaylistExportPreflightError};

/// Крупная категория причины пропуска, достаточная для короткого текста в UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistExportSkipReason {
    /// Локальный путь нельзя обратимо записать (не UTF-8, чужая ОС, относительный).
    LocalPathNotRepresentable,
    /// У ссылки нет стабильного адреса, который можно сохранить в файл.
    NoStableWebLink,
    /// Остальные причины (форматные ограничения, не относящиеся к двум выше).
    Other,
}

impl PlaylistExportSkipReason {
    /// Сворачивает подробную причину preflight-а в категорию для UI.
    pub const fn from_ineligible(reason: PlaylistExportIneligible) -> Self {
        match reason {
            PlaylistExportIneligible::NonUtf8LocalPath
            | PlaylistExportIneligible::ForeignPlatformPath
            | PlaylistExportIneligible::RelativeLocalPath
            | PlaylistExportIneligible::UnrepresentableNativePath => {
                Self::LocalPathNotRepresentable
            }
            PlaylistExportIneligible::LocatorPolicy(_) => Self::NoStableWebLink,
            PlaylistExportIneligible::Cue(_) => Self::Other,
        }
    }
}

/// Сколько записей пропущено по каждой категории.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaylistExportSkipCounts {
    /// Пропущено из-за локального пути.
    pub local_path_not_representable: usize,
    /// Пропущено из-за ссылки без стабильного адреса.
    pub no_stable_web_link: usize,
    /// Пропущено по прочим причинам.
    pub other: usize,
}

impl PlaylistExportSkipCounts {
    /// Собирает счётчики по списку пропущенных записей.
    pub fn from_skipped(skipped: &[PlaylistExportPreflightError]) -> Self {
        let mut counts = Self::default();
        for skipped_item in skipped {
            match PlaylistExportSkipReason::from_ineligible(skipped_item.reason()) {
                PlaylistExportSkipReason::LocalPathNotRepresentable => {
                    counts.local_path_not_representable += 1;
                }
                PlaylistExportSkipReason::NoStableWebLink => counts.no_stable_web_link += 1,
                PlaylistExportSkipReason::Other => counts.other += 1,
            }
        }
        counts
    }

    /// Общее число пропущенных записей.
    pub const fn total(self) -> usize {
        self.local_path_not_representable + self.no_stable_web_link + self.other
    }
}
