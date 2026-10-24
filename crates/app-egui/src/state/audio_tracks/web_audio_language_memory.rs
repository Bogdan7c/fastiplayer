//! Память языка звука «для этого видео» (web-видео с дубляжами).
//!
//! # Что запоминается
//! Язык, который пользователь выбрал в меню «♪» или клавишей `B`, и ТОЛЬКО после того, как
//! смена реально установилась (`lifecycle_bridge`): отказ, устаревший выбор и отмена в память
//! не попадают. Выбор оригинала тоже пишется — он перекрывает ранее запомненный дубляж,
//! иначе следующее открытие вернуло бы старый дубляж.
//!
//! # Восстановление
//! После обычного открытия yt-dlp-видео (не переоткрытия внутри одной линии: смена качества
//! и версии звука) память сверяется со списком языков НОВОГО источника. Если запомненный язык
//! есть в списке и не совпадает с играющим, ставится ровно один запрос на смену через тот же
//! same-item switch (`AudioLanguagePurpose::MemoryRestore`).
//!
//! Почему смена после установки, а не стартовое предпочтение: языки известны только из
//! снимка форматов, который строится при открытии, а пара «видео + звук нужного языка»
//! собирается там же из активного видео. Стартовое предпочтение потребовало бы второго
//! прохода выбора форматов внутри открытия (и новой ветки в планировщике). Цена выбранного
//! пути — один дополнительный reopen (около секунды) в начале видео, и оригинальный звук
//! успевает прозвучать, если открытие шло с автозапуском.
//!
//! # Границы повторов
//! Запрос живёт в `AppState.requested_web_audio_switch` и применяется в `frame_prepare`.
//! Пока открытие не завершилось (`Busy`, нет активной идентичности), запрос переносится на
//! следующий кадр, но не более [`RESTORE_ATTEMPT_BUDGET`] раз. Любой другой исход (старт,
//! «уже играет», устаревшее поколение, ошибка) снимает запрос навсегда: цикла нет, повторная
//! попытка возможна только при следующем открытии видео.

use media_source_open::web_media_open::audio_languages::{
    WebAudioLanguageAction, WebAudioLanguageChoice,
};
use playlist_state::RememberedWebAudioLanguage;
use render_shell_api::RenderShell;

use super::web_version_switch::WebAudioSwitchRequest;
use crate::playlist_runtime::PlaylistRuntime;
use crate::state::same_item_candidate_switch::{
    AudioLanguagePurpose, SameItemSwitchError, UrlSidebarActionApplyOutcome,
};
use crate::state::{ActiveMediaSource, AppState};

/// Сколько кадров запрос восстановления ждёт завершения открытия, прежде чем сдаться.
pub(in crate::state) const RESTORE_ATTEMPT_BUDGET: u16 = 300;

/// Что делать с запомненным языком при открытии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LanguageRestorePlan {
    /// Для видео ничего не запомнено.
    NothingRemembered,
    /// Запомненный язык уже играет.
    AlreadyActive,
    /// Запомненного языка нет в списке этого источника: остаётся язык по умолчанию.
    LanguageAbsent,
    /// Переключиться на этот язык.
    Switch(WebAudioLanguageAction),
}

/// Чистое решение по памяти и списку языков источника.
pub(super) fn plan_language_restore(
    remembered: Option<&RememberedWebAudioLanguage>,
    choices: &[WebAudioLanguageChoice],
) -> LanguageRestorePlan {
    let Some(remembered) = remembered else {
        return LanguageRestorePlan::NothingRemembered;
    };
    match choices
        .iter()
        .find(|choice| remembered.matches_tag(choice.language.as_str()))
    {
        None => LanguageRestorePlan::LanguageAbsent,
        Some(choice) if choice.is_active => LanguageRestorePlan::AlreadyActive,
        Some(choice) => LanguageRestorePlan::Switch(choice.action),
    }
}

/// Язык действия в виде значения памяти; `None` — действия нет в списке или тег не годится.
pub(super) fn language_to_remember(
    choices: &[WebAudioLanguageChoice],
    action: WebAudioLanguageAction,
) -> Option<RememberedWebAudioLanguage> {
    let choice = choices.iter().find(|choice| choice.action == action)?;
    match RememberedWebAudioLanguage::new(choice.language.as_str().to_owned()) {
        Ok(language) => Some(language),
        Err(error) => {
            tracing::debug!(%error, "The audio language cannot be remembered");
            None
        }
    }
}

/// Ошибки старта, нормальные, пока открытие не завершилось: стоит подождать кадр.
///
/// `Stale` входит сюда осознанно: до публикации нового media активным остаётся прежний
/// источник, и действие нового видео для него «устарело». Если media потом сменилось по-
/// настоящему, действие остаётся устаревшим до конца бюджета и ничего не делает.
const fn is_open_still_settling(error: &SameItemSwitchError) -> bool {
    matches!(
        error,
        SameItemSwitchError::Stale
            | SameItemSwitchError::Busy
            | SameItemSwitchError::MissingActiveIdentity
            | SameItemSwitchError::MissingBinding
            | SameItemSwitchError::UnsupportedSource
    )
}

/// Нужна ли ещё одна попытка после этого исхода; `None` — запрос снимается навсегда.
///
/// Старт, «уже играет», окончательный отказ и исчерпанный бюджет попыток снимают запрос:
/// именно это не даёт восстановлению зациклиться после неудачи.
pub(super) fn follow_up_after_restore_attempt(
    outcome: &Result<UrlSidebarActionApplyOutcome, SameItemSwitchError>,
    action: WebAudioLanguageAction,
    attempts_left: u16,
) -> Option<WebAudioSwitchRequest> {
    match outcome {
        Err(error) if is_open_still_settling(error) && attempts_left > 1 => {
            Some(WebAudioSwitchRequest::RestoreRemembered {
                action,
                attempts_left: attempts_left - 1,
            })
        }
        Ok(UrlSidebarActionApplyOutcome::Started | UrlSidebarActionApplyOutcome::NoChange)
        | Err(_) => None,
    }
}

impl AppState {
    /// Только что установлено новое media: если память знает другой язык — ставит запрос.
    ///
    /// Зовётся из пути «новое открытие» (не из переоткрытия внутри линии), поэтому смена
    /// качества и версии звука язык не сбрасывают и не зацикливаются.
    pub(in crate::state) fn schedule_remembered_web_audio_language(
        &mut self,
        source: &ActiveMediaSource,
    ) {
        let Some(intent) = source.web_intent() else {
            return;
        };
        let choices = intent.web_audio_language_choices();
        if choices.is_empty() {
            return;
        }
        let remembered = self.remembered_web_audio_language(source);
        if let LanguageRestorePlan::Switch(action) =
            plan_language_restore(remembered.as_ref(), &choices)
        {
            self.requested_web_audio_switch = Some(WebAudioSwitchRequest::RestoreRemembered {
                action,
                attempts_left: RESTORE_ATTEMPT_BUDGET,
            });
            self.mark_pending_worker_redraw();
        }
    }

    /// Применяет запрос восстановления: старт, ожидание завершения открытия или отказ.
    pub(super) fn apply_requested_language_restore(
        &mut self,
        action: WebAudioLanguageAction,
        attempts_left: u16,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &mut dyn RenderShell,
    ) {
        let outcome = self.apply_audio_language_action(
            action,
            AudioLanguagePurpose::MemoryRestore,
            playlist_runtime,
            renderer,
        );
        if let Err(error) = &outcome {
            tracing::debug!(%error, "The remembered audio language did not start");
        }
        if let Some(retry) = follow_up_after_restore_attempt(&outcome, action, attempts_left) {
            self.requested_web_audio_switch = Some(retry);
            self.mark_pending_worker_redraw();
        }
    }
}
