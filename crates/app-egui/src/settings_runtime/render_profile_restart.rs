//! Выбор профиля рендера (`render.profile`): «Applies after restart» и кнопка «Restart now».
//!
//! Решение владельца (gles2-renderer, сессия 09, №37): смена профиля не пересоздаёт рендер
//! вживую. Значение сохраняется, строка рядом с полем говорит «Applies after restart», а когда
//! отличается уже СОХРАНЁННОЕ значение, рядом появляется кнопка «Restart now».
//!
//! Модуль владеет двумя вещами:
//! - профилем, с которым запущен процесс ([`StartupRenderProfile`]): он запоминается один раз
//!   при создании [`SettingsRuntime`] и не меняется ни Apply-ом, ни
//!   пересозданием рендера внутри семейства (последнее профиль не затрагивает);
//! - чистыми решениями: что показать у поля и принять ли действие `RestartNow`.

use std::collections::BTreeSet;

use fastiplayer_config::{AppConfig, RenderProfile};
use settings_core::{SettingId, SettingsResult};

use super::SettingsRuntime;
use crate::app_instance::RelaunchRequest;
use crate::settings_ui::{RestartOffer, SettingsUiField};

/// Стабильный id настройки профиля рендера (см. `RenderConfig::profile`).
pub(super) const RENDER_PROFILE_SETTING_ID: &str = "render.profile";

/// Строка статуса у поля профиля; пользовательский текст на английском.
pub(super) const APPLIES_AFTER_RESTART_STATUS: &str = "Applies after restart";

/// Есть ли в черновике настроек неприменённые правки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DraftChanges {
    /// Черновик совпадает с сохранёнными настройками.
    None,

    /// Есть неприменённые правки (они потеряются при перезапуске).
    Unapplied,
}

/// Занят ли runtime настроек многокадровым применением.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApplyActivity {
    /// Применение не идёт.
    Idle,

    /// Идёт многокадровое применение: закрывать приложение посреди него нельзя.
    InProgress,
}

/// Что делать с действием `RestartNow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RestartNowDecision {
    /// Принять: запросить перезапуск после штатного выхода.
    Accept,

    /// Отклонить: сохранённый профиль совпадает с тем, с которым запущен процесс.
    NothingPending,

    /// Отклонить: в черновике есть неприменённые правки, перезапуск их потерял бы.
    UnappliedChanges,

    /// Отклонить: идёт применение настроек.
    ApplyInProgress,
}

/// Профиль рендера, с которым запущен текущий процесс (из конфига на старте).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StartupRenderProfile(RenderProfile);

impl StartupRenderProfile {
    /// Запоминает профиль из стартового конфига.
    pub(super) fn from_config(config: &AppConfig) -> Self {
        Self(config.render.profile)
    }

    /// Что показать у поля профиля при данных черновике и сохранённом значении.
    ///
    /// Статус виден, если отличается черновик ИЛИ сохранённое значение. Кнопка — только когда
    /// отличается сохранённое значение и перезапуск не потеряет правок (см. [`Self::decide_restart_now`]).
    pub(super) fn field_presentation(
        &self,
        draft_profile: RenderProfile,
        committed_profile: RenderProfile,
        draft_changes: DraftChanges,
    ) -> ProfileFieldPresentation {
        let differs = draft_profile != self.0 || committed_profile != self.0;
        let offer = if committed_profile != self.0 && draft_changes == DraftChanges::None {
            RestartOffer::Available
        } else {
            RestartOffer::None
        };
        ProfileFieldPresentation {
            status: differs.then_some(APPLIES_AFTER_RESTART_STATUS),
            offer,
        }
    }

    /// Решает судьбу действия `RestartNow` (UI могло устареть относительно runtime).
    pub(super) fn decide_restart_now(
        &self,
        committed_profile: RenderProfile,
        draft_changes: DraftChanges,
        apply_activity: ApplyActivity,
    ) -> RestartNowDecision {
        if committed_profile == self.0 {
            RestartNowDecision::NothingPending
        } else if draft_changes == DraftChanges::Unapplied {
            RestartNowDecision::UnappliedChanges
        } else if apply_activity == ApplyActivity::InProgress {
            RestartNowDecision::ApplyInProgress
        } else {
            RestartNowDecision::Accept
        }
    }
}

/// Что рисовать у поля профиля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProfileFieldPresentation {
    /// Строка статуса или `None`.
    pub(super) status: Option<&'static str>,

    /// Предлагать ли кнопку «Restart now».
    pub(super) offer: RestartOffer,
}

impl ProfileFieldPresentation {
    /// Применяет представление к полю модели.
    pub(super) fn apply_to(self, field: SettingsUiField) -> SettingsUiField {
        let field = field.with_restart_offer(self.offer);
        match self.status {
            Some(status) => field.with_status(status),
            None => field,
        }
    }
}

impl SettingsRuntime {
    /// Есть ли неприменённые правки, по набору изменённых полей.
    pub(super) fn draft_changes_of(dirty_ids: &BTreeSet<SettingId>) -> DraftChanges {
        if dirty_ids.is_empty() {
            DraftChanges::None
        } else {
            DraftChanges::Unapplied
        }
    }

    /// Просил ли пользователь перезапуск; shell читает это после каждого кадра.
    #[must_use]
    pub(crate) const fn relaunch_request(&self) -> RelaunchRequest {
        self.relaunch_request
    }

    /// Обрабатывает `RestartNow`: принимает только когда есть что перезапускать и ничего не
    /// потеряется; иначе действие отбрасывается с записью в лог (UI мог устареть).
    pub(super) fn handle_restart_now(&mut self) -> SettingsResult<bool> {
        let draft_changes = Self::draft_changes_of(
            &self
                .controller
                .diff()?
                .changes()
                .iter()
                .map(|change| change.id.clone())
                .collect(),
        );
        let apply_activity = if self.pending_apply.is_some() || self.route_sequence.is_some() {
            ApplyActivity::InProgress
        } else {
            ApplyActivity::Idle
        };
        let decision = self.startup_render_profile.decide_restart_now(
            self.controller.committed().render.profile,
            draft_changes,
            apply_activity,
        );
        match decision {
            RestartNowDecision::Accept => {
                tracing::info!("Restart requested from Settings (render profile changed)");
                self.relaunch_request = RelaunchRequest::AfterExit;
                self.invalidate_ui_model();
                Ok(true)
            }
            rejected => {
                tracing::warn!(?rejected, "Restart now ignored");
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn startup(profile: RenderProfile) -> StartupRenderProfile {
        let mut config = AppConfig::default();
        config.render.profile = profile;
        StartupRenderProfile::from_config(&config)
    }

    #[test]
    fn matching_values_show_nothing() {
        let presentation = startup(RenderProfile::Auto).field_presentation(
            RenderProfile::Auto,
            RenderProfile::Auto,
            DraftChanges::None,
        );

        assert_eq!(presentation.status, None);
        assert_eq!(presentation.offer, RestartOffer::None);
    }

    #[test]
    fn edited_draft_shows_status_without_button() {
        let presentation = startup(RenderProfile::Auto).field_presentation(
            RenderProfile::Vulkan,
            RenderProfile::Auto,
            DraftChanges::Unapplied,
        );

        assert_eq!(presentation.status, Some(APPLIES_AFTER_RESTART_STATUS));
        assert_eq!(presentation.offer, RestartOffer::None);
    }

    #[test]
    fn saved_difference_shows_status_and_button() {
        let presentation = startup(RenderProfile::Auto).field_presentation(
            RenderProfile::Vulkan,
            RenderProfile::Vulkan,
            DraftChanges::None,
        );

        assert_eq!(presentation.status, Some(APPLIES_AFTER_RESTART_STATUS));
        assert_eq!(presentation.offer, RestartOffer::Available);
    }

    #[test]
    fn saved_difference_with_other_unapplied_edits_hides_button() {
        let presentation = startup(RenderProfile::Auto).field_presentation(
            RenderProfile::Vulkan,
            RenderProfile::Vulkan,
            DraftChanges::Unapplied,
        );

        assert_eq!(presentation.status, Some(APPLIES_AFTER_RESTART_STATUS));
        assert_eq!(presentation.offer, RestartOffer::None);
    }

    #[test]
    fn restart_now_is_accepted_only_for_a_pending_clean_idle_runtime() {
        let startup = startup(RenderProfile::Auto);
        let decide =
            |committed, draft, activity| startup.decide_restart_now(committed, draft, activity);

        assert_eq!(
            decide(RenderProfile::Auto, DraftChanges::None, ApplyActivity::Idle),
            RestartNowDecision::NothingPending
        );
        assert_eq!(
            decide(
                RenderProfile::Vulkan,
                DraftChanges::Unapplied,
                ApplyActivity::Idle
            ),
            RestartNowDecision::UnappliedChanges
        );
        assert_eq!(
            decide(
                RenderProfile::Vulkan,
                DraftChanges::None,
                ApplyActivity::InProgress
            ),
            RestartNowDecision::ApplyInProgress
        );
        assert_eq!(
            decide(
                RenderProfile::Vulkan,
                DraftChanges::None,
                ApplyActivity::Idle
            ),
            RestartNowDecision::Accept
        );
    }
}
