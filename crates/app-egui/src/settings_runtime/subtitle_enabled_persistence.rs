//! Запоминание «субтитры были включены» в `config.toml` (`subtitles.enabled`).
//!
//! Пользователь нажал C или кликнул CC, и следующий запуск должен начаться с тем же
//! выбором. Схема та же, что у громкости (`user_audio_level_persistence`): изменение не
//! пишется сразу, а через паузу после последнего нажатия и при выходе, чтобы серия кликов
//! дала одну запись. Сама запись идёт через обычную runtime settings transaction, поэтому
//! работают все защиты config-а: validation, atomic replace и режим «только в памяти».
//!
//! Источник изменения — владелец субтитров (`take_subtitle_enabled_preference_change`):
//! он отдаёт только решения пользователя, автовыбор дорожки изменения не создаёт.

use super::*;
use crate::state::SubtitleVisibility;

/// Пауза после последнего переключения перед записью config-а (как у громкости).
pub(super) const SUBTITLE_ENABLED_PERSIST_DEBOUNCE: Duration = Duration::from_secs(3);

/// Stable setting id.
const SUBTITLES_ENABLED_SETTING_ID: &str = "subtitles.enabled";

/// Решение пользователя, которое ещё не записано.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PendingSubtitleEnabled {
    /// Последнее выбранное состояние.
    visibility: SubtitleVisibility,
    /// Когда писать.
    persist_at: Instant,
}

/// Результат попытки записи без смешения «нечего делать» и ошибки transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubtitleEnabledFlushOutcome {
    /// Записывать нечего или пауза ещё не прошла.
    NoPending,
    /// Config синхронизирован с выбором пользователя (или запись была no-op).
    Succeeded,
    /// Transaction вернула typed failure; показ субтитров не затронут.
    Failed,
}

impl SubtitleEnabledFlushOutcome {
    /// Завершённая попытка меняет status/config и требует перерисовки.
    #[must_use]
    pub(crate) fn needs_redraw(self) -> bool {
        self != Self::NoPending
    }
}

impl SettingsRuntime {
    /// Запоминает новое решение пользователя; `None` (изменений не было) ничего не делает.
    ///
    /// Если решение совпало с тем, что уже в config, отложенная запись отменяется: писать то же
    /// самое незачем. Возвращает `true`, если отложенное состояние изменилось.
    pub(crate) fn record_subtitle_enabled_change(
        &mut self,
        change: Option<SubtitleVisibility>,
        now: Instant,
    ) -> bool {
        let Some(visibility) = change else {
            return false;
        };
        let committed =
            SubtitleVisibility::from_enabled(self.controller.committed().subtitles.enabled);
        let new_pending = (visibility != committed).then_some(PendingSubtitleEnabled {
            visibility,
            persist_at: now + SUBTITLE_ENABLED_PERSIST_DEBOUNCE,
        });
        let changed = self.pending_subtitle_enabled != new_pending;
        self.pending_subtitle_enabled = new_pending;
        changed
    }

    /// Ближайший момент записи для idle event loop.
    #[must_use]
    pub(crate) fn next_subtitle_enabled_persist_deadline(&self) -> Option<Instant> {
        if self.is_apply_in_flight() {
            // Как у громкости: запись ждёт конец применения настроек.
            return None;
        }
        self.pending_subtitle_enabled
            .map(|pending| pending.persist_at)
    }

    /// Записывает решение, только когда пауза после него уже прошла.
    pub(crate) fn flush_due_subtitle_enabled<A>(
        &mut self,
        now: Instant,
        runtime_adapter: &mut A,
    ) -> SettingsResult<SubtitleEnabledFlushOutcome>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        match self.pending_subtitle_enabled {
            Some(pending) if now >= pending.persist_at && !self.is_apply_in_flight() => {
                self.flush_pending_subtitle_enabled(runtime_adapter)
            }
            _ => Ok(SubtitleEnabledFlushOutcome::NoPending),
        }
    }

    /// Принудительно записывает отложенное решение (выход, suspend).
    pub(crate) fn flush_pending_subtitle_enabled<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<SubtitleEnabledFlushOutcome>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let Some(pending) = self.pending_subtitle_enabled.take() else {
            return Ok(SubtitleEnabledFlushOutcome::NoPending);
        };
        let report = self.commit_runtime_setting_with_runtime_adapter(
            RuntimeSettingCommitRequest::new(
                SUBTITLES_ENABLED_SETTING_ID,
                SettingValue::Bool(pending.visibility.is_enabled()),
            ),
            runtime_adapter,
        )?;
        if matches!(
            report.final_state,
            ApplyFinalState::FullyApplied | ApplyFinalState::NoChanges
        ) {
            return Ok(SubtitleEnabledFlushOutcome::Succeeded);
        }
        // Controller уже откатил transaction; показ субтитров остаётся таким, как выбрал
        // пользователь, просто config не изменился.
        self.invalidate_ui_model();
        self.status = status_from_apply_report(&self.setting_labels(), &report);
        tracing::error!(
            final_state = ?report.final_state,
            attempted_enabled = pending.visibility.is_enabled(),
            "Failed to keep the subtitles choice in config"
        );
        Ok(SubtitleEnabledFlushOutcome::Failed)
    }
}
