//! Запись ответа на одноразовое предложение «подключить браузеры» (`browser_bridge.offer_on_startup`).
//!
//! Предложение показывается при запуске, если значение `true`. Любой ответ пользователя
//! (обе кнопки или крестик) гасит его навсегда: приложение пишет `false` **одиночным
//! runtime-коммитом** по id. Коммит строится от последнего committed-документа, поэтому
//! незавершённые правки открытого окна настроек (draft) не попадают в файл и не теряются:
//! заменяется только это одно поле.
//!
//! Применять значение живьём не нужно (его читает только старт приложения); маршрут
//! `browser_session` принимает коммит без действий (`AppliesAfterRestart`).

use super::*;

/// Stable id настройки в реестре.
const OFFER_ON_STARTUP_SETTING_ID: &str = "browser_bridge.offer_on_startup";

/// Итог записи ответа без смешения «уже было записано» и ошибки транзакции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserOfferDismissalOutcome {
    /// `false` записан в config (или в память при режиме «только сессия»).
    Recorded,
    /// В config уже стояло `false`: запись не понадобилась.
    AlreadyDismissed,
    /// Транзакция откатилась; причина в статусе настроек и журнале, предложение
    /// покажется снова при следующем запуске.
    Failed,
}

impl SettingsRuntime {
    /// Записывает `browser_bridge.offer_on_startup = false` одиночным runtime-коммитом.
    ///
    /// Ошибка уровня реестра/контроллера (например, идёт другая транзакция Apply)
    /// возвращается как `Err`; откат самой транзакции — `Ok(Failed)`.
    pub(crate) fn dismiss_browser_connection_offer<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<BrowserOfferDismissalOutcome>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        if !self.controller.committed().browser_bridge.offer_on_startup {
            return Ok(BrowserOfferDismissalOutcome::AlreadyDismissed);
        }
        let report = self.commit_runtime_setting_with_runtime_adapter(
            RuntimeSettingCommitRequest::new(
                OFFER_ON_STARTUP_SETTING_ID,
                SettingValue::Bool(false),
            ),
            runtime_adapter,
        )?;
        if matches!(
            report.final_state,
            ApplyFinalState::FullyApplied | ApplyFinalState::NoChanges
        ) {
            return Ok(BrowserOfferDismissalOutcome::Recorded);
        }
        self.invalidate_ui_model();
        self.status = status_from_apply_report(&self.setting_labels(), &report);
        tracing::error!(
            final_state = ?report.final_state,
            "Failed to keep the browser connection offer answer in config"
        );
        Ok(BrowserOfferDismissalOutcome::Failed)
    }
}
