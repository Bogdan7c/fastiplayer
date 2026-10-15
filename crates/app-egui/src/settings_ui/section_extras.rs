//! Дополнительные строки отдельных секций настроек, которых нет в descriptor-метаданных.
//!
//! Метаданные описывают только значения конфига, а некоторым секциям нужна кнопка действия
//! над живым приложением (сейчас: «Edit appearance on video…» в секции субтитров). Модуль
//! держит такие строки на одном месте и не пускает egui-логику в рисование полей:
//! `layout` лишь вызывает [`show_extras_for_section`] под заголовком секции.
//!
//! Модуль ничего не знает о плеере: факты о приложении приходят готовым значением
//! [`SettingsHostFacts`], а наружу уходит [`SettingsUiAction`].

use egui::Ui;
use settings_core::SettingSectionId;

use super::{SettingsUiAction, SettingsUiCommandState};

/// Id секции субтитров в descriptor-метаданных (`subtitles.*`).
const SUBTITLES_SECTION_ID: &str = "subtitles";

/// Подпись кнопки входа в правку внешнего вида на видео.
const EDIT_APPEARANCE_LABEL: &str = "Edit appearance on video…";
/// Подсказка, когда видео нет.
const HINT_OPEN_A_VIDEO_FIRST: &str = "Open a video first";
/// Подсказка, когда в настройках есть неприменённые правки.
const HINT_APPLY_OR_CANCEL_FIRST: &str = "Apply or cancel your changes first";

/// Есть ли установленное видео, на котором можно что-то править.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstalledMediaAvailability {
    /// Видео открыто и установлено.
    Present,
    /// Видео нет.
    #[default]
    Absent,
}

/// Факты о приложении, нужные дополнительным строкам; собирает владелец окна настроек.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SettingsHostFacts {
    /// Установлено ли видео.
    pub installed_media: InstalledMediaAvailability,
}

/// Состояние кнопки входа в правку на видео.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleAppearanceEntry {
    /// Можно войти в режим правки.
    Enabled,
    /// Выключена: видео нет.
    DisabledNoMedia,
    /// Выключена: в черновике настроек есть неприменённые изменения.
    DisabledUnappliedChanges,
}

impl SubtitleAppearanceEntry {
    /// Состояние кнопки из фактов приложения и состояния командных кнопок настроек.
    ///
    /// Если не выполнено сразу два условия, показывается более базовое: «сначала откройте видео».
    #[must_use]
    pub fn resolve(host: SettingsHostFacts, command_state: SettingsUiCommandState) -> Self {
        if host.installed_media == InstalledMediaAvailability::Absent {
            Self::DisabledNoMedia
        } else if command_state.has_dirty_fields {
            Self::DisabledUnappliedChanges
        } else {
            Self::Enabled
        }
    }

    /// Текст подсказки для выключенной кнопки; у включённой подсказки нет.
    #[must_use]
    pub fn disabled_hint(self) -> Option<&'static str> {
        match self {
            Self::Enabled => None,
            Self::DisabledNoMedia => Some(HINT_OPEN_A_VIDEO_FIRST),
            Self::DisabledUnappliedChanges => Some(HINT_APPLY_OR_CANCEL_FIRST),
        }
    }
}

/// Рисует дополнительные строки секции (если они у неё есть) и добавляет действия в `actions`.
pub(super) fn show_extras_for_section(
    ui: &mut Ui,
    section: &SettingSectionId,
    host: SettingsHostFacts,
    command_state: SettingsUiCommandState,
    actions: &mut Vec<SettingsUiAction>,
) {
    if section.as_str() == SUBTITLES_SECTION_ID {
        show_subtitle_appearance_entry(
            ui,
            SubtitleAppearanceEntry::resolve(host, command_state),
            actions,
        );
    }
}

/// Кнопка «Edit appearance on video…» с видимой причиной, если она выключена.
fn show_subtitle_appearance_entry(
    ui: &mut Ui,
    entry: SubtitleAppearanceEntry,
    actions: &mut Vec<SettingsUiAction>,
) {
    ui.horizontal(|ui| {
        let response = ui.add_enabled(
            entry == SubtitleAppearanceEntry::Enabled,
            egui::Button::new(EDIT_APPEARANCE_LABEL),
        );
        if response.clicked() && entry == SubtitleAppearanceEntry::Enabled {
            actions.push(SettingsUiAction::EditSubtitleAppearanceOnVideo);
        }
        // Причина видна рядом с кнопкой, а не только в подсказке при наведении.
        if let Some(hint) = entry.disabled_hint() {
            ui.weak(hint);
        }
    });
    ui.add_space(8.0);
}
