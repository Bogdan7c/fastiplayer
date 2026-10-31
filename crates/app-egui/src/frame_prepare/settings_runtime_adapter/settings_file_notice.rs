//! Показ уведомлений «общей доски» настроек в окне.
//!
//! Решение «показывать ли» принимает `SettingsRuntime` (одно уведомление на новую проблему
//! файла или на неудачное принятие изменений соседа); адаптер кадра только отдаёт текст
//! центру уведомлений своего окна — у адаптера есть `AppState` этого окна.

use super::FrameSettingsRuntimeAdapter;
use crate::settings_runtime::SettingsFileNotice;

impl FrameSettingsRuntimeAdapter<'_> {
    /// Показывает информационный тост (8 с, ×): настройки при этом не менялись.
    pub(in crate::frame_prepare) fn show_settings_file_notice(
        &mut self,
        notice: SettingsFileNotice,
    ) {
        self.app_state.notify_info(notice.message());
    }

    /// Показывает «применено, но в файл пока не записано» с причиной от хранилища.
    pub(in crate::frame_prepare) fn show_settings_save_notice(
        &mut self,
        notice: std::sync::Arc<str>,
    ) {
        self.app_state.notify_info(notice);
    }

    /// Снимает плашки «только на сеанс»: запись в файл снова разрешена.
    pub(in crate::frame_prepare) fn withdraw_settings_file_write_notices(&mut self) {
        self.app_state.withdraw_settings_file_write_notices();
    }
}
