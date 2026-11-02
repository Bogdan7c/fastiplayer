//! Связка `AppState` с браузерной интеграцией (мост, сессия 06).
//!
//! Владелец состояния — `crate::browser_integration::BrowserIntegration` (его нет в окне
//! браузерного режима). Здесь только маршрут: пробуждение фоновой операции, предложение
//! при первом запуске, действия панели из окна настроек и ответ на предложение, который
//! нужно записать в config (запись выполняет покадровый цикл: `AppState` не владеет
//! `SettingsRuntime`).

use std::sync::Arc;
use std::time::Instant;

use super::AppState;
use crate::browser_integration::routing::{self, RoutingEffect};
use crate::browser_integration::{
    AnswerOutcome, BrowserIntegration, OfferAnswer, OfferVerdict, PollOutcome,
};
use crate::settings_ui::SettingsUiAction;
use crate::settings_ui::browser_panel::BrowserPanelView;
use crate::settings_ui::section_extras::SectionRequest;
use crate::state::{NotificationId, SidebarSection};

impl AppState {
    /// Забирает результат фоновой операции; готовое предложение превращает в toast.
    /// `true` — видимое состояние изменилось.
    pub(crate) fn poll_browser_integration(&mut self) -> bool {
        let Some(integration) = self.browser_integration.as_mut() else {
            return false;
        };
        let mut changed = integration.poll() == PollOutcome::Changed;
        if integration.take_startup_offer_verdict() == Some(OfferVerdict::Show) {
            let toast_id = self
                .notifications
                .notify_browser_connection_offer(Instant::now());
            integration.mark_offer_shown(toast_id);
            changed = true;
        }
        changed
    }

    /// Модель панели «Browser integration» (пустая, если интеграции нет).
    pub(crate) fn browser_panel_view(&self) -> Arc<BrowserPanelView> {
        self.browser_integration
            .as_ref()
            .map_or_else(Arc::default, BrowserIntegration::panel_view)
    }

    /// Просьба показать раздел настроек (после «Set up…»).
    pub(crate) fn browser_section_request(&self) -> Option<SectionRequest> {
        self.browser_integration
            .as_ref()
            .and_then(BrowserIntegration::section_request)
    }

    /// Забирает действия панели из списка действий настроек (до `SettingsRuntime`).
    pub(crate) fn route_browser_integration_settings_actions(
        &mut self,
        actions: &mut Vec<SettingsUiAction>,
    ) {
        let Some(integration) = self.browser_integration.as_mut() else {
            return;
        };
        match routing::route_settings_actions(integration, actions) {
            RoutingEffect::Nothing => {}
            RoutingEffect::VisibleChange => self.mark_pending_worker_redraw(),
            RoutingEffect::SettingsPanelRequested => {
                // `Open` уже в списке действий (черновик правки); саму панель показывает
                // контроллер сайдбара — тем же путём, что клик по шестерёнке, но без
                // «повторный клик закрывает».
                let _outcome = self.sidebar_controller.show(SidebarSection::Settings);
                self.mark_pending_worker_redraw();
            }
        }
    }

    /// Ответ пользователя на уведомление `id`; не относящееся к предложению игнорируется.
    pub(in crate::state) fn answer_browser_offer(
        &mut self,
        id: NotificationId,
        answer: OfferAnswer,
    ) {
        let Some(integration) = self.browser_integration.as_mut() else {
            return;
        };
        if integration.answer_offer(id, answer) == AnswerOutcome::Recorded {
            self.mark_pending_worker_redraw();
        }
    }

    /// Ответ на предложение, который нужно записать в config. Отдаётся один раз.
    pub(crate) fn take_browser_offer_answer(&mut self) -> Option<OfferAnswer> {
        self.browser_integration
            .as_mut()
            .and_then(BrowserIntegration::take_offer_answer)
    }
}
