//! Запись пользовательского стиля субтитров («Готово» в режиме правки на видео).
//!
//! В отличие от «субтитры включены» (`subtitle_enabled_persistence`), стиль пишется сразу, без
//! паузы: это одно осознанное действие пользователя, а не серия кликов. Все поля стиля уходят
//! ОДНОЙ runtime-транзакцией (`commit_runtime_settings_with_runtime_adapter`): одна валидация,
//! одна запись `config.toml`, одно применение маршрута `subtitles.apply`. Результат - явный
//! enum, чтобы вызывающий мог показать уведомление и вернуть показ на закоммиченный стиль.

use super::*;

/// Итог записи стиля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubtitleStyleCommitOutcome {
    /// Стиль в конфиге (или записывать было нечего / значения совпали с конфигом).
    Saved,
    /// Транзакция откатилась или вернула ошибку: конфиг не изменён, причина в статусе настроек.
    Failed,
}

impl SettingsRuntime {
    /// Коммитит значения стиля одной транзакцией.
    pub(crate) fn commit_subtitle_style<A>(
        &mut self,
        requests: Vec<RuntimeSettingCommitRequest>,
        runtime_adapter: &mut A,
    ) -> SubtitleStyleCommitOutcome
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        match self.commit_runtime_settings_with_runtime_adapter(requests, runtime_adapter) {
            Ok(RuntimeCommitOutcome::NothingRequested) => SubtitleStyleCommitOutcome::Saved,
            Ok(RuntimeCommitOutcome::Transaction(report)) => {
                if matches!(
                    report.final_state,
                    ApplyFinalState::FullyApplied | ApplyFinalState::NoChanges
                ) {
                    return SubtitleStyleCommitOutcome::Saved;
                }
                // Controller уже откатил транзакцию; config остался прежним.
                self.invalidate_ui_model();
                self.status = status_from_apply_report(&self.setting_labels(), &report);
                tracing::error!(
                    final_state = ?report.final_state,
                    "Failed to keep the subtitle style in config"
                );
                SubtitleStyleCommitOutcome::Failed
            }
            Err(error) => {
                self.report_runtime_error("Could not save subtitle style", error);
                SubtitleStyleCommitOutcome::Failed
            }
        }
    }
}
