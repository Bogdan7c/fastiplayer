//! Связка `AppState` с правкой внешнего вида субтитров на видео.
//!
//! # Границы
//! - Сессия, черновик, образец и показ - у `SubtitleOwner` (`state/subtitles/owner_appearance_edit.rs`).
//! - Здесь то, что требует `AppState`: перерисовка, уведомление об ошибке и выдача списка
//!   значений для записи в конфиг.
//! - Саму запись выполняет покадровый цикл: `AppState` не владеет `SettingsRuntime`. Цикл
//!   забирает [`AppState::take_subtitle_style_commit`], коммитит все поля стиля ОДНОЙ
//!   транзакцией (`SettingsRuntime::commit_runtime_settings_with_runtime_adapter`) и при
//!   отказе вызывает [`AppState::report_subtitle_style_save_failed`].
//!
//! # Неявные выходы
//! Видео исчезло - владелец сам кладёт стиль в очередь записи (в `clear_media`), так что
//! правка сохраняется тем же путём, что и «Готово». Смена видео на другое (следующий в
//! плейлисте) сессию не прерывает.

use settings_core::RuntimeSettingCommitRequest;

use super::AppState;
use super::subtitles::SubtitleOwner;
use super::subtitles::{
    AppearanceEdit, AppearanceEditBegin, AppearanceEditChange, AppearanceEditFinish,
    AppearanceEditOutcome, AppearanceEditView,
};
use crate::settings_ui::SettingsUiAction;
use crate::settings_ui::section_extras::{InstalledMediaAvailability, SettingsHostFacts};
use crate::ui::subtitle_appearance_edit::SubtitleAppearanceEditAction;

/// Сообщение, если стиль субтитров не удалось записать в конфиг.
const STYLE_SAVE_FAILED_MESSAGE: &str = "Could not save subtitle style.";

impl AppState {
    /// Применяет изменение к черновику вживую.
    pub(crate) fn apply_subtitle_appearance_edit(
        &mut self,
        edit: AppearanceEdit,
    ) -> AppearanceEditChange {
        let change = self.subtitles.apply_appearance_edit(edit);
        if change == AppearanceEditChange::Changed {
            self.mark_pending_worker_redraw();
        }
        change
    }

    /// «Сброс»: черновик становится стилем по умолчанию, режим остаётся открытым.
    pub(crate) fn reset_subtitle_appearance_draft(&mut self) -> AppearanceEditChange {
        let change = self.subtitles.reset_appearance_draft();
        if change == AppearanceEditChange::Changed {
            self.mark_pending_worker_redraw();
        }
        change
    }

    /// Завершает правку: `Save` ставит запись в очередь, `Cancel` возвращает прежний стиль.
    pub(crate) fn finish_subtitle_appearance_edit(
        &mut self,
        finish: AppearanceEditFinish,
    ) -> AppearanceEditOutcome {
        let outcome = self.subtitles.finish_appearance_edit(finish);
        if outcome != AppearanceEditOutcome::NotEditing {
            self.mark_pending_worker_redraw();
        }
        outcome
    }

    /// Снимок для UI правки (`None`, если правки нет).
    pub(crate) fn subtitle_appearance_edit_view(&self) -> Option<AppearanceEditView> {
        self.subtitles.appearance_edit_view()
    }

    /// Идёт ли правка внешнего вида.
    pub(crate) fn is_editing_subtitle_appearance(&self) -> bool {
        self.subtitles.is_editing_appearance()
    }

    /// Значения стиля, которые нужно записать в конфиг одной транзакцией. Отдаются один раз.
    pub(crate) fn take_subtitle_style_commit(
        &mut self,
    ) -> Option<Vec<RuntimeSettingCommitRequest>> {
        self.subtitles
            .take_style_save_request()
            .map(|style| style.commit_requests())
    }

    /// Запись стиля не удалась: показывается закоммиченный стиль, пользователь видит уведомление.
    pub(crate) fn report_subtitle_style_save_failed(&mut self) {
        self.subtitles.discard_unsaved_style();
        self.notify_transient(STYLE_SAVE_FAILED_MESSAGE);
    }
}

impl AppState {
    /// Факты приложения для окна настроек (кнопка «Edit appearance on video…»).
    pub(crate) fn settings_host_facts(&self) -> SettingsHostFacts {
        SettingsHostFacts {
            installed_media: if self.subtitles.has_installed_media() {
                InstalledMediaAvailability::Present
            } else {
                InstalledMediaAvailability::Absent
            },
        }
    }

    /// Применяет намерения панели и жестов режима правки в порядке их появления.
    pub(crate) fn apply_subtitle_appearance_edit_actions(
        &mut self,
        actions: Vec<SubtitleAppearanceEditAction>,
    ) {
        for action in actions {
            match action {
                SubtitleAppearanceEditAction::Edit(edit) => {
                    let _change = self.apply_subtitle_appearance_edit(edit);
                }
                SubtitleAppearanceEditAction::StartMove => {
                    // Меняется только сдвиг слоя: перерисовка нужна, перерастеризация - нет.
                    if self.subtitles.start_appearance_layer_move() == AppearanceEditChange::Changed
                    {
                        self.mark_pending_worker_redraw();
                    }
                }
                SubtitleAppearanceEditAction::Reset => {
                    let _change = self.reset_subtitle_appearance_draft();
                }
                SubtitleAppearanceEditAction::Finish(finish) => {
                    let _outcome = self.finish_subtitle_appearance_edit(finish);
                }
            }
        }
    }

    /// Связывает действия окна настроек с режимом правки (вызывать после прохода egui, до
    /// передачи действий в `SettingsRuntime`); правила - в [`route_settings_actions`].
    pub(crate) fn route_subtitle_appearance_edit_settings_actions(
        &mut self,
        actions: &mut Vec<SettingsUiAction>,
    ) {
        if route_settings_actions(&mut self.subtitles, actions) == SettingsRouting::EditStarted {
            self.mark_pending_worker_redraw();
        }
    }

    /// Граница жизненного цикла (suspend, выход): открытая правка завершается сохранением, и
    /// значения стиля для записи отдаются одним списком. Запись выполняет вызывающий код.
    pub(crate) fn take_subtitle_style_commit_for_lifecycle_boundary(
        &mut self,
    ) -> Option<Vec<RuntimeSettingCommitRequest>> {
        take_style_commit_for_lifecycle_boundary(&mut self.subtitles)
    }
}

/// Чем закончилась маршрутизация действий окна настроек.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsRouting {
    /// Режим правки только что открыт.
    EditStarted,
    /// Режим правки не открывался (мог закрыться неявно).
    EditNotStarted,
}

/// Правила связи окна настроек и режима правки.
///
/// - «Edit appearance on video…»: действие заменяется на `Cancel` (закрывает панель
///   настроек; черновик чист - иначе кнопка выключена) и открывается режим правки. Если
///   видео пропало между кадрами, действие просто отбрасывается и настройки остаются.
/// - Открытие настроек во время правки (шестерёнка, `Open`/`ToggleOpen`): неявный выход,
///   как «Done» - правка сохраняется (решение владельца 6).
pub(super) fn route_settings_actions(
    owner: &mut SubtitleOwner,
    actions: &mut Vec<SettingsUiAction>,
) -> SettingsRouting {
    let entry_requested = actions.contains(&SettingsUiAction::EditSubtitleAppearanceOnVideo);
    let settings_reopened = actions.iter().any(|action| {
        matches!(
            action,
            SettingsUiAction::Open | SettingsUiAction::ToggleOpen
        )
    });
    if settings_reopened {
        let _outcome = owner.finish_appearance_edit(AppearanceEditFinish::Save);
    }
    if !entry_requested {
        return SettingsRouting::EditNotStarted;
    }
    let begin = owner.begin_appearance_edit();
    actions.retain(|action| *action != SettingsUiAction::EditSubtitleAppearanceOnVideo);
    match begin {
        AppearanceEditBegin::Started | AppearanceEditBegin::AlreadyActive => {
            actions.push(SettingsUiAction::Cancel);
        }
        AppearanceEditBegin::NoMedia => {}
    }
    if begin == AppearanceEditBegin::Started {
        SettingsRouting::EditStarted
    } else {
        SettingsRouting::EditNotStarted
    }
}

/// Граница жизненного цикла: открытая правка заканчивается сохранением; значения стиля для
/// записи отдаются один раз (или `None`, если писать нечего).
pub(super) fn take_style_commit_for_lifecycle_boundary(
    owner: &mut SubtitleOwner,
) -> Option<Vec<RuntimeSettingCommitRequest>> {
    let _outcome = owner.finish_appearance_edit(AppearanceEditFinish::Save);
    owner
        .take_style_save_request()
        .map(|style| style.commit_requests())
}
