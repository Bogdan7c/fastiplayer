//! Предложение «подключить браузеры» при первом запуске (решение 159).
//!
//! Модуль владеет двумя вещами: чистым правилом «показывать ли предложение» и
//! жизненным циклом одного предложения (ждём обзор → вердикт → показано → ответ).
//! Сам toast рисует центр уведомлений, запись отказа в config делает покадровый цикл
//! через `SettingsRuntime::dismiss_browser_connection_offer`.

use browser_bridge_install::BrowserBridgeStatus;

use super::operations::SurveyResult;
use crate::state::NotificationId;

/// Значение настройки `browser_bridge.offer_on_startup` на момент запуска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OfferSetting {
    /// Предлагать при запуске.
    Enabled,
    /// Пользователь уже ответил: не предлагать.
    Disabled,
}

impl OfferSetting {
    /// Из значения настройки.
    pub(crate) fn from_config_value(offer_on_startup: bool) -> Self {
        if offer_on_startup {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

/// Почему предложение не показывается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OfferSkipReason {
    /// Настройка выключена.
    DisabledInSettings,
    /// Обзор не удался (платформа не поддерживается).
    SurveyFailed,
    /// Эта копия плеера не может подключать браузеры: предлагать нечего.
    SourceUnusable,
    /// Нет браузера, который можно подключить.
    NothingToConnect,
    /// Уже есть подключённый браузер: пользователь это уже сделал.
    AlreadyConnected,
}

/// Вердикт правила предложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OfferVerdict {
    /// Показать предложение.
    Show,
    /// Не показывать.
    Skip(OfferSkipReason),
}

/// Правило: предлагаем, если настройка включена, есть браузер в `NotConnected`/`Outdated`
/// и ни один браузер не подключён.
///
/// «Подключён» здесь шире, чем `Connected`: `ConnectedToAnotherCopy` и
/// `NeedsFlatpakPermission` тоже означают, что подключение уже делали (другая копия плеера
/// или Flatpak без разрешения), поэтому навязывать его заново не нужно.
pub(crate) fn offer_verdict(setting: OfferSetting, survey: &SurveyResult) -> OfferVerdict {
    if setting == OfferSetting::Disabled {
        return OfferVerdict::Skip(OfferSkipReason::DisabledInSettings);
    }
    let Ok(survey) = survey else {
        return OfferVerdict::Skip(OfferSkipReason::SurveyFailed);
    };
    if survey.source.is_err() {
        return OfferVerdict::Skip(OfferSkipReason::SourceUnusable);
    }
    let already_set_up = survey.browsers.iter().any(|report| {
        matches!(
            report.status,
            BrowserBridgeStatus::Connected
                | BrowserBridgeStatus::ConnectedToAnotherCopy { .. }
                | BrowserBridgeStatus::NeedsFlatpakPermission
        )
    });
    if already_set_up {
        return OfferVerdict::Skip(OfferSkipReason::AlreadyConnected);
    }
    let can_be_connected = survey.browsers.iter().any(|report| {
        matches!(
            report.status,
            BrowserBridgeStatus::NotConnected | BrowserBridgeStatus::Outdated
        )
    });
    if can_be_connected {
        OfferVerdict::Show
    } else {
        OfferVerdict::Skip(OfferSkipReason::NothingToConnect)
    }
}

/// Как пользователь ответил на предложение. Любой ответ гасит его навсегда.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OfferAnswer {
    /// «Set up…»: открыть настройки на разделе браузеров.
    SetUp,
    /// «Not now».
    NotNow,
    /// Закрыто крестиком.
    Closed,
}

/// Куда продвинулось предложение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OfferPhase {
    /// Ждём первый обзор, чтобы вынести вердикт.
    AwaitingSurvey(OfferSetting),
    /// Вердикт вынесен, приложение его ещё не забрало.
    VerdictReady(OfferVerdict),
    /// Вердикт забран; toast ещё не зарегистрирован (или предлагать нечего).
    Delivered,
    /// Toast на экране под этим id.
    Shown(NotificationId),
    /// Предложение закрыто (ответ получен или не нужно).
    Closed,
}

/// Исход попытки записать ответ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnswerOutcome {
    /// Это было наше предложение: ответ принят.
    Recorded,
    /// Уведомление не наше (или предложение уже закрыто): ничего не изменилось.
    NotTheOffer,
}

/// Жизненный цикл одного предложения.
#[derive(Debug)]
pub(super) struct OfferLifecycle {
    phase: OfferPhase,
    /// Ответ, который ещё не записан в config.
    pending_answer: Option<OfferAnswer>,
}

impl OfferLifecycle {
    /// Предложение, ожидающее первого обзора.
    pub(super) fn awaiting_survey(setting: OfferSetting) -> Self {
        Self {
            phase: OfferPhase::AwaitingSurvey(setting),
            pending_answer: None,
        }
    }

    /// Первый обзор готов: выносим вердикт (повторные обзоры его не меняют).
    pub(super) fn observe_survey(&mut self, survey: &SurveyResult) {
        if let OfferPhase::AwaitingSurvey(setting) = self.phase {
            self.phase = OfferPhase::VerdictReady(offer_verdict(setting, survey));
        }
    }

    /// Забирает вердикт ровно один раз.
    pub(super) fn take_verdict(&mut self) -> Option<OfferVerdict> {
        let OfferPhase::VerdictReady(verdict) = self.phase else {
            return None;
        };
        self.phase = match verdict {
            OfferVerdict::Show => OfferPhase::Delivered,
            OfferVerdict::Skip(_) => OfferPhase::Closed,
        };
        Some(verdict)
    }

    /// Toast показан под этим id.
    pub(super) fn mark_shown(&mut self, id: NotificationId) {
        if self.phase == OfferPhase::Delivered {
            self.phase = OfferPhase::Shown(id);
        }
    }

    /// Записывает ответ на уведомление `id`.
    pub(super) fn answer(&mut self, id: NotificationId, answer: OfferAnswer) -> AnswerOutcome {
        if self.phase != OfferPhase::Shown(id) {
            return AnswerOutcome::NotTheOffer;
        }
        self.phase = OfferPhase::Closed;
        self.pending_answer = Some(answer);
        AnswerOutcome::Recorded
    }

    /// Забирает ответ, который нужно записать в config (один раз).
    pub(super) fn take_answer(&mut self) -> Option<OfferAnswer> {
        self.pending_answer.take()
    }
}
