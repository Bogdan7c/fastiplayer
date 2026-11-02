//! Браузерная интеграция в обычном окне плеера (мост «браузер качает — мы играем», сессия 06).
//!
//! # Архитектура
//! - [`BrowserIntegration`] владеет установщиком [`BrowserBridgeInstaller`], ОДНОЙ
//!   выполняющейся операцией, последним обзором, итогом последней операции и жизненным
//!   циклом предложения при первом запуске ([`offer`]).
//! - Установщик блокирующий, поэтому каждая операция идёт в своём фоновом потоке; результат
//!   возвращается через `owner_mailbox` с пробуждением `AppWakeOwner::BrowserIntegration`
//!   (окно просыпается и на паузе). Двух операций одновременно нет: пока одна идёт, новые
//!   команды отклоняются, а запрошенный обзор откладывается.
//! - После каждой операции владелец сам запускает свежий обзор: статус на экране всегда
//!   сравнен с диском заново.
//! - Наружу отдаётся готовая модель вида ([`BrowserIntegration::panel_view`]); UI установщика
//!   не видит. Конструируется только в обычном режиме: режим `--browser-session` его не
//!   создаёт (порт пробуждения выдаёт только обычная оболочка).
//!
//! # Инварианты
//! - Операция записи стартует только по явному запросу пользователя; сам по себе владелец
//!   делает лишь обзоры (чтение).
//! - Ответ на предложение принимается один раз и только для того toast-а, который показали.

mod offer;
mod operations;
mod panel_view;
pub(crate) mod routing;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::thread;

use browser_bridge_install::{
    BrowserBridgeInstaller, BrowserTargetId, DesktopSessionBusReloader, InstallEnvironment,
    InstallSource, RealFlatpakTool,
};
use tracing::{debug, warn};

pub(crate) use self::offer::{AnswerOutcome, OfferAnswer, OfferSetting, OfferVerdict};
use self::operations::{BrowserOperation, OperationResult, SurveyResult, run_operation};
use self::panel_view::{PanelInputs, build_panel_view};
use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::settings_ui::browser_panel::{BrowserPanelActivity, BrowserPanelView};
use crate::settings_ui::section_extras::{BROWSER_INTEGRATION_SECTION_ID, SectionRequest};
use crate::state::NotificationId;

/// Имя фонового потока операции.
const OPERATION_THREAD_NAME: &str = "browser-integration";

/// Чем кончилась просьба выполнить операцию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestOutcome {
    /// Операция запущена в фоне.
    Started,
    /// Идёт другая операция: обзор отложен и выполнится сразу после неё.
    QueuedAfterRunning,
    /// Идёт другая операция: команда отклонена (кнопки в это время выключены).
    RefusedBusy,
    /// Установщика нет (например, не задан HOME): выполнять нечего.
    RefusedNoInstaller,
}

/// Изменилось ли видимое состояние после `poll`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollOutcome {
    /// Ничего нового.
    Unchanged,
    /// Модель вида изменилась: нужна перерисовка.
    Changed,
}

/// Какого действия окна настроек ждёт приложение после ответа на предложение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsOpenRequest {
    /// Открыть настройки на разделе «Browser integration».
    BrowserIntegrationSection,
}

/// Установщик или причина его отсутствия.
enum Backend {
    Ready(Arc<BrowserBridgeInstaller>),
    Unavailable(String),
}

/// Владелец браузерной интеграции окна.
pub(crate) struct BrowserIntegration {
    backend: Backend,
    wake_port: AppWakePort,
    /// Приёмник результата выполняющейся операции (не более одной).
    running: Option<OwnerMailboxReceiver<(), OperationResult>>,
    /// Обзор запрошен во время другой операции.
    survey_queued: bool,
    /// Последний обзор.
    survey: Option<SurveyResult>,
    /// Итог последней операции записи (предупреждения/ошибка), пока не начнётся следующая.
    operation_notes: Vec<String>,
    /// Готовая модель вида (пересобирается при любом изменении состояния).
    view: Arc<BrowserPanelView>,
    offer: offer::OfferLifecycle,
    /// Номер последней просьбы открыть раздел (0 — просьб не было).
    section_request_serial: u64,
    /// «Set up…» нажато, настройки ещё не открыты.
    pending_settings_open: Option<SettingsOpenRequest>,
}

impl BrowserIntegration {
    /// Боевой владелец: окружение процесса, текущий исполняемый файл, настоящие `flatpak` и
    /// шина. Сразу запускает стартовый обзор (для предложения и для первого открытия панели).
    pub(crate) fn start_production(wake_port: AppWakePort, offer_setting: OfferSetting) -> Self {
        let backend = match InstallEnvironment::from_process_env() {
            Ok(environment) => Backend::Ready(Arc::new(BrowserBridgeInstaller::new(
                environment,
                InstallSource::from_current_exe(),
                Box::new(RealFlatpakTool::new()),
                Box::new(DesktopSessionBusReloader::new()),
            ))),
            Err(error) => Backend::Unavailable(error.to_string()),
        };
        Self::start_with_backend(backend, wake_port, offer_setting)
    }

    /// Владелец с готовым установщиком (тесты подставляют фейки и временный HOME).
    #[cfg(test)]
    pub(crate) fn start_with_installer(
        installer: BrowserBridgeInstaller,
        wake_port: AppWakePort,
        offer_setting: OfferSetting,
    ) -> Self {
        Self::start_with_backend(
            Backend::Ready(Arc::new(installer)),
            wake_port,
            offer_setting,
        )
    }

    fn start_with_backend(
        backend: Backend,
        wake_port: AppWakePort,
        offer_setting: OfferSetting,
    ) -> Self {
        let mut integration = Self {
            backend,
            wake_port,
            running: None,
            survey_queued: false,
            survey: None,
            operation_notes: Vec::new(),
            view: Arc::new(BrowserPanelView::default()),
            offer: offer::OfferLifecycle::awaiting_survey(offer_setting),
            section_request_serial: 0,
            pending_settings_open: None,
        };
        let _outcome = integration.request_survey();
        integration.refresh_view();
        integration
    }

    /// Просит свежий обзор (при открытии настроек, после операций).
    pub(crate) fn request_survey(&mut self) -> RequestOutcome {
        if self.running.is_some() {
            self.survey_queued = true;
            return RequestOutcome::QueuedAfterRunning;
        }
        self.start(BrowserOperation::Survey)
    }

    /// Просит подключить браузер к этой копии плеера (явный клик пользователя).
    pub(crate) fn request_connect(&mut self, target: &BrowserTargetId) -> RequestOutcome {
        self.start_change(BrowserOperation::Connect(target.clone()))
    }

    /// Просит убрать свои файлы браузера (явный клик пользователя).
    pub(crate) fn request_disconnect(&mut self, target: &BrowserTargetId) -> RequestOutcome {
        self.start_change(BrowserOperation::Disconnect(target.clone()))
    }

    /// Просит выдать Flatpak-браузеру доступ к шине (явный клик пользователя).
    pub(crate) fn request_allow_flatpak_bus_access(
        &mut self,
        target: &BrowserTargetId,
    ) -> RequestOutcome {
        self.start_change(BrowserOperation::AllowFlatpakBusAccess(target.clone()))
    }

    /// Неблокирующе забирает результат операции; при завершении запускает следующий обзор.
    pub(crate) fn poll(&mut self) -> PollOutcome {
        let Some(running) = self.running.as_ref() else {
            return PollOutcome::Unchanged;
        };
        let drain = running.drain();
        let result = if let Some(result) = drain.completion {
            Some(result)
        } else if drain.producer_disconnected_without_completion {
            warn!("Browser integration thread ended without an answer");
            Some(OperationResult::Change {
                notes: vec!["The background task stopped unexpectedly".to_owned()],
            })
        } else {
            None
        };
        let Some(result) = result else {
            return PollOutcome::Unchanged;
        };
        self.running = None;
        self.apply_result(result);
        PollOutcome::Changed
    }

    /// Текущая модель вида панели.
    pub(crate) fn panel_view(&self) -> Arc<BrowserPanelView> {
        Arc::clone(&self.view)
    }

    /// Вердикт по предложению: выдаётся один раз, когда готов первый обзор.
    pub(crate) fn take_startup_offer_verdict(&mut self) -> Option<OfferVerdict> {
        self.offer.take_verdict()
    }

    /// Предложение показано toast-ом с этим id.
    pub(crate) fn mark_offer_shown(&mut self, id: NotificationId) {
        self.offer.mark_shown(id);
    }

    /// Пользователь ответил на уведомление `id`. «Set up…» дополнительно просит открыть раздел.
    pub(crate) fn answer_offer(
        &mut self,
        id: NotificationId,
        answer: OfferAnswer,
    ) -> AnswerOutcome {
        let outcome = self.offer.answer(id, answer);
        if outcome == AnswerOutcome::Recorded && answer == OfferAnswer::SetUp {
            self.pending_settings_open = Some(SettingsOpenRequest::BrowserIntegrationSection);
            self.section_request_serial += 1;
        }
        outcome
    }

    /// Ответ на предложение, который нужно записать в config (один раз).
    pub(crate) fn take_offer_answer(&mut self) -> Option<OfferAnswer> {
        self.offer.take_answer()
    }

    /// Просьба открыть настройки (один раз).
    pub(crate) fn take_settings_open_request(&mut self) -> Option<SettingsOpenRequest> {
        self.pending_settings_open.take()
    }

    /// Какую секцию настроек показать (держится, пока layout не выполнит её по номеру).
    pub(crate) fn section_request(&self) -> Option<SectionRequest> {
        (self.section_request_serial > 0).then_some(SectionRequest {
            section: BROWSER_INTEGRATION_SECTION_ID,
            serial: self.section_request_serial,
        })
    }

    /// Команда записи: отклоняется, пока идёт другая операция.
    fn start_change(&mut self, operation: BrowserOperation) -> RequestOutcome {
        if self.running.is_some() {
            return RequestOutcome::RefusedBusy;
        }
        self.start(operation)
    }

    /// Запускает операцию в фоновом потоке.
    fn start(&mut self, operation: BrowserOperation) -> RequestOutcome {
        let Backend::Ready(installer) = &self.backend else {
            return RequestOutcome::RefusedNoInstaller;
        };
        let installer = Arc::clone(installer);
        let (publisher, receiver) = owner_mailbox::<(), OperationResult>(self.wake_port.clone());
        let thread_operation = operation.clone();
        let spawn_result = thread::Builder::new()
            .name(OPERATION_THREAD_NAME.to_owned())
            .spawn(move || {
                let result = run_operation(&installer, &thread_operation);
                match publisher.publish_completion(result) {
                    Ok(WakeDelivery::EventLoopClosed) => {
                        debug!("Event loop closed; browser integration result is dropped");
                    }
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(CompletionPublishError::AlreadyPublished) => {
                        warn!("Browser integration published its result twice");
                    }
                }
            });
        match spawn_result {
            Ok(_detached_handle) => {
                if operation != BrowserOperation::Survey {
                    self.operation_notes.clear();
                }
                self.running = Some(receiver);
                self.refresh_view();
                RequestOutcome::Started
            }
            Err(error) => {
                warn!(%error, "Could not start the browser integration thread");
                self.operation_notes = vec!["Could not start a background task".to_owned()];
                self.refresh_view();
                RequestOutcome::RefusedNoInstaller
            }
        }
    }

    /// Принимает результат завершённой операции.
    fn apply_result(&mut self, result: OperationResult) {
        match result {
            OperationResult::Survey(survey) => {
                self.offer.observe_survey(&survey);
                self.survey = Some(survey);
            }
            OperationResult::Change { notes } => {
                self.operation_notes = notes;
                // Состояние на диске изменилось: перечитываем.
                self.survey_queued = true;
            }
        }
        self.refresh_view();
        if std::mem::take(&mut self.survey_queued) {
            let _outcome = self.start(BrowserOperation::Survey);
        }
    }

    /// Пересобирает модель вида из текущего состояния.
    fn refresh_view(&mut self) {
        let installer_problem = match &self.backend {
            Backend::Ready(_) => None,
            Backend::Unavailable(problem) => Some(problem.as_str()),
        };
        let activity = if self.running.is_some() {
            BrowserPanelActivity::Working
        } else {
            BrowserPanelActivity::Idle
        };
        self.view = Arc::new(build_panel_view(&PanelInputs {
            survey: self.survey.as_ref(),
            installer_problem,
            activity,
            operation_notes: &self.operation_notes,
        }));
    }
}
