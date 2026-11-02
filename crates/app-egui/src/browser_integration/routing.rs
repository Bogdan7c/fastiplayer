//! Маршрутизация действий окна настроек, относящихся к браузерной интеграции.
//!
//! Вызывается после прохода egui и ДО передачи действий в `SettingsRuntime`:
//! - клики панели («Connect», «Disconnect», «Allow», «Install extension», «Cancel») забираются целиком, поэтому окно
//!   настроек их не видит и не закрывается;
//! - открытие настроек (`Open`/`ToggleOpen`) просит свежий обзор;
//! - ответ «Set up…» на предложение превращается в `Open` (раздел выбирает
//!   `section_request`).

use browser_bridge_install::BrowserTargetId;

use super::{BrowserIntegration, SettingsOpenRequest};
use crate::settings_ui::SettingsUiAction;
use crate::settings_ui::browser_panel::BrowserCommand;

/// Что сделал маршрутизатор (для перерисовки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutingEffect {
    /// Видимое состояние могло измениться.
    VisibleChange,
    /// Пользователь нажал «Set up…»: кроме `Open` в списке действий вызывающий обязан
    /// ПОКАЗАТЬ панель настроек в сайдбаре. `SettingsUiAction::Open` только начинает
    /// правку черновика, а видимостью сайдбара владеет `SidebarController` (живая
    /// проверка 06: без этого «Set up…» гасил предложение, но настройки не появлялись).
    SettingsPanelRequested,
    /// Ничего не произошло.
    Nothing,
}

/// Применяет правила к списку действий кадра.
pub(crate) fn route_settings_actions(
    owner: &mut BrowserIntegration,
    actions: &mut Vec<SettingsUiAction>,
) -> RoutingEffect {
    let mut effect = RoutingEffect::Nothing;
    let settings_opened = actions.iter().any(|action| {
        matches!(
            action,
            SettingsUiAction::Open | SettingsUiAction::ToggleOpen
        )
    });
    if settings_opened {
        let _outcome = owner.request_survey();
        effect = RoutingEffect::VisibleChange;
    }

    let mut kept_actions = Vec::with_capacity(actions.len());
    for action in std::mem::take(actions) {
        let SettingsUiAction::BrowserIntegration { target_id, command } = action else {
            kept_actions.push(action);
            continue;
        };
        let target = BrowserTargetId::new(target_id);
        let outcome = match command {
            BrowserCommand::Connect => owner.request_connect(&target),
            BrowserCommand::Disconnect => owner.request_disconnect(&target),
            BrowserCommand::AllowFlatpakBusAccess => {
                owner.request_allow_flatpak_bus_access(&target)
            }
            BrowserCommand::InstallExtension => owner.request_install_extension(&target),
            BrowserCommand::CancelExtensionInstall => {
                owner.request_cancel_extension_install(&target)
            }
        };
        tracing::debug!(?command, ?outcome, "Browser integration command routed");
        effect = RoutingEffect::VisibleChange;
    }
    *actions = kept_actions;

    if let Some(SettingsOpenRequest::BrowserIntegrationSection) = owner.take_settings_open_request()
    {
        actions.push(SettingsUiAction::Open);
        // Свежий обзор: пользователь только что согласился подключать браузеры.
        let _outcome = owner.request_survey();
        effect = RoutingEffect::SettingsPanelRequested;
    }
    effect
}
