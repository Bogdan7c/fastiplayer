//! Панель «Browser integration» в настройках: готовая модель вида и её рисование.
//!
//! Модуль чистый (как весь `settings_ui`): он не знает ни про установщик, ни про файлы
//! браузеров. Приложение собирает [`BrowserPanelView`] из обзора установщика
//! (`crate::browser_integration::panel_view`), а клики возвращает как
//! [`SettingsUiAction::BrowserIntegration`] с id цели. Окно настроек этими действиями
//! не закрывается: их перехватывает владелец приложения до `SettingsRuntime`.

use egui::Ui;

use super::SettingsUiAction;

/// Что браузерная панель просит сделать с одной целью.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserCommand {
    /// Подключить (или переподключить) браузер к этой копии плеера.
    Connect,
    /// Убрать свои файлы подключения.
    Disconnect,
    /// Выдать Flatpak-браузеру доступ к имени моста на шине.
    AllowFlatpakBusAccess,
    /// Поставить экстеншен в этот браузер (подсказка или страница магазина).
    InstallExtension,
    /// Отменить ещё не случившуюся установку экстеншена (убрать нашу подсказку).
    /// Уже установленный экстеншен убирает человек в браузере.
    CancelExtensionInstall,
}

/// Доступна ли кнопка строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserButtonAvailability {
    /// Кнопку можно нажать.
    Enabled,
    /// Кнопка выключена (например, у этой копии плеера нет посредника для Connect).
    Disabled,
}

/// Кнопка строки браузера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserRowButton {
    /// Подпись («Connect», «Reconnect to this copy», ...).
    pub label: &'static str,
    /// Что произойдёт по клику.
    pub command: BrowserCommand,
    /// Можно ли нажать.
    pub availability: BrowserButtonAvailability,
}

/// Ссылка на справку, открывается в браузере по умолчанию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserHelpLink {
    /// Текст ссылки.
    pub label: &'static str,
    /// Адрес справки.
    pub url: &'static str,
}

/// Строка экстеншена внутри строки браузера (сессия 07a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRowView {
    /// Статус человеческим языком («Extension: installed», ...).
    pub status_text: String,
    /// Кнопки по порядку: не более одной основной, затем «Cancel».
    pub buttons: Vec<BrowserRowButton>,
    /// Приглушённые пояснения под статусом.
    pub notes: Vec<String>,
    /// Справочная ссылка, если она нужна статусу.
    pub help_link: Option<BrowserHelpLink>,
}

/// Одна строка: найденный браузер, его статус и доступные действия.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserRowView {
    /// ID цели для действий (непрозрачная строка установщика).
    pub target_id: String,
    /// Имя браузера.
    pub name: String,
    /// Подпись упаковки: «package» или «Flatpak».
    pub packaging_label: &'static str,
    /// Путь для этой цели не проверялся вживую.
    pub not_verified: bool,
    /// Статус человеческим языком.
    pub status_text: String,
    /// Кнопки по порядку.
    pub buttons: Vec<BrowserRowButton>,
    /// Справочная ссылка, если она нужна статусу.
    pub help_link: Option<BrowserHelpLink>,
    /// Состояние экстеншена в этом браузере (`None` — обзор экстеншена не знает браузер).
    pub extension: Option<ExtensionRowView>,
}

/// Идёт ли поиск браузеров.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserScanState {
    /// Первый обзор ещё не закончился.
    #[default]
    Scanning,
    /// Есть результат обзора (возможно, пустой).
    Finished,
}

/// Работает ли фоновая операция.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserPanelActivity {
    /// Ничего не выполняется: кнопки доступны.
    #[default]
    Idle,
    /// Идёт операция: кнопки выключены, виден «Working…».
    Working,
}

/// Модель вида всей панели.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowserPanelView {
    /// Идёт ли поиск.
    pub scan: BrowserScanState,
    /// Идёт ли операция.
    pub activity: BrowserPanelActivity,
    /// Общие строки над списком (проблема источника, сбой `flatpak`, нет установщика).
    pub global_notes: Vec<String>,
    /// Найденные браузеры.
    pub rows: Vec<BrowserRowView>,
    /// Итог последней операции (предупреждения и ошибки) под списком.
    pub operation_notes: Vec<String>,
}

/// Текст при пустом списке.
const NO_BROWSERS_MESSAGE: &str = "No browsers found";
/// Текст, пока обзор не готов.
const SCANNING_MESSAGE: &str = "Looking for browsers…";
/// Текст, пока работает операция.
const WORKING_MESSAGE: &str = "Working…";
/// Метка непроверенной цели.
const NOT_VERIFIED_LABEL: &str = "not verified";

impl BrowserPanelView {
    /// Что сказать вместо списка, если строк нет.
    #[must_use]
    pub fn list_message(&self) -> Option<&'static str> {
        match (self.scan, self.rows.is_empty()) {
            (BrowserScanState::Scanning, true) => Some(SCANNING_MESSAGE),
            (BrowserScanState::Finished, true) => Some(NO_BROWSERS_MESSAGE),
            (_, false) => None,
        }
    }

    /// Действие по клику на кнопку строки.
    #[must_use]
    pub fn action_for(row: &BrowserRowView, button: &BrowserRowButton) -> SettingsUiAction {
        SettingsUiAction::BrowserIntegration {
            target_id: row.target_id.clone(),
            command: button.command,
        }
    }
}

/// Рисует панель и добавляет действия кликов в `actions`.
pub(super) fn show_browser_panel(
    ui: &mut Ui,
    panel: &BrowserPanelView,
    actions: &mut Vec<SettingsUiAction>,
) {
    for note in &panel.global_notes {
        ui.colored_label(ui.visuals().warn_fg_color, note);
    }
    if let Some(message) = panel.list_message() {
        ui.label(message);
    }
    for row in &panel.rows {
        show_row(ui, row, panel.activity, actions);
    }
    if panel.activity == BrowserPanelActivity::Working {
        ui.weak(WORKING_MESSAGE);
    }
    for note in &panel.operation_notes {
        ui.colored_label(ui.visuals().warn_fg_color, note);
    }
    ui.add_space(8.0);
}

/// Рисует одну строку: имя, статус, кнопки и справочную ссылку.
fn show_row(
    ui: &mut Ui,
    row: &BrowserRowView,
    activity: BrowserPanelActivity,
    actions: &mut Vec<SettingsUiAction>,
) {
    ui.push_id(&row.target_id, |ui| {
        ui.horizontal(|ui| {
            ui.strong(&row.name);
            ui.weak(row.packaging_label);
            if row.not_verified {
                ui.weak(NOT_VERIFIED_LABEL);
            }
        });
        ui.label(&row.status_text);
        show_buttons(ui, &row.buttons, row.help_link, row, activity, actions);
        if let Some(extension) = &row.extension {
            ui.label(&extension.status_text);
            show_buttons(
                ui,
                &extension.buttons,
                extension.help_link,
                row,
                activity,
                actions,
            );
            for note in &extension.notes {
                ui.weak(note);
            }
        }
        ui.add_space(6.0);
    });
}

/// Рисует ряд кнопок и справочную ссылку; клики превращает в действия строки `row`.
fn show_buttons(
    ui: &mut Ui,
    buttons: &[BrowserRowButton],
    help_link: Option<BrowserHelpLink>,
    row: &BrowserRowView,
    activity: BrowserPanelActivity,
    actions: &mut Vec<SettingsUiAction>,
) {
    if buttons.is_empty() && help_link.is_none() {
        return;
    }
    ui.horizontal(|ui| {
        for button in buttons {
            let enabled = activity == BrowserPanelActivity::Idle
                && button.availability == BrowserButtonAvailability::Enabled;
            if ui
                .add_enabled(enabled, egui::Button::new(button.label))
                .clicked()
                && enabled
            {
                actions.push(BrowserPanelView::action_for(row, button));
            }
        }
        if let Some(link) = help_link {
            ui.hyperlink_to(link.label, link.url);
        }
    });
}
