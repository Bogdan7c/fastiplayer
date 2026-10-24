//! Смена языка звука (дубляж) yt-dlp-видео через общий same-item switch.
//!
//! Язык не является ни catalog target-ом, ни component-вариантом: его разбирает владелец
//! extractor-состояния (`WebMediaSourceIntent::audio_language_switch_request`). Здесь только
//! запуск общей транзакции — позиция и Play/Pause сохраняются, старое воспроизведение живёт
//! до успешной установки нового, действия с чужим поколением отвергаются как `Stale`.

use media_source_open::web_media_open::audio_languages::WebAudioLanguageAction;
use render_shell_api::RenderShell;

use super::{AppState, SameItemSwitchError, SameItemSwitchKind, UrlSidebarActionApplyOutcome};
use crate::media_open::{
    WebMediaOpenSettings, WebMediaSelectionSwitchIntent, WebMediaSelectionSwitchResolution,
    WebMediaSourceIntent,
};
use crate::playlist_runtime::PlaylistRuntime;

/// Что именно просят у владельца источника: общий выбор либо смену языка звука.
///
/// Нужен, чтобы `start_resolved_same_item_switch` не знал, каким способом источник
/// превращает запрос в переоткрытие, и не раздувал `WebMediaSelectionSwitchIntent`.
pub(super) enum SameItemSelectionRequest {
    /// Catalog target либо component-вариант (прежний путь).
    Selection(WebMediaSelectionSwitchIntent),
    /// Другой язык звука того же ролика.
    AudioLanguage(WebAudioLanguageAction),
}

impl From<WebMediaSelectionSwitchIntent> for SameItemSelectionRequest {
    fn from(intent: WebMediaSelectionSwitchIntent) -> Self {
        Self::Selection(intent)
    }
}

impl SameItemSelectionRequest {
    /// Просит владельца источника разобрать запрос.
    pub(super) fn resolve(
        self,
        source: &WebMediaSourceIntent,
        settings: WebMediaOpenSettings,
    ) -> WebMediaSelectionSwitchResolution {
        match self {
            Self::Selection(intent) => source.selection_switch_request(intent, settings),
            Self::AudioLanguage(action) => source.audio_language_switch_request(action, settings),
        }
    }
}

/// Кто просит сменить язык: от этого зависят память выбора и адресат сообщения об отказе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::state) enum AudioLanguagePurpose {
    /// Пользователь выбрал язык в меню «♪» или клавишей `B`. После успешной установки язык
    /// записывается в память «для этого видео» (`remember_as`; `None` — тег не годится для
    /// памяти), отказ показывается toast-ом меню.
    UserChoice {
        remember_as: Option<playlist_state::RememberedWebAudioLanguage>,
    },
    /// Автоматическое восстановление запомненного языка при открытии. В память не пишется
    /// (там это уже лежит), отказ молчит и не повторяется: только запись в лог.
    MemoryRestore,
}

impl AppState {
    /// Запускает смену языка звука (выбор пользователя или восстановление из памяти).
    pub(in crate::state) fn apply_audio_language_action(
        &mut self,
        action: WebAudioLanguageAction,
        purpose: AudioLanguagePurpose,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &mut dyn RenderShell,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        let active_source = self.preflight_same_item_switch()?;
        self.start_resolved_same_item_switch(
            active_source,
            SameItemSelectionRequest::AudioLanguage(action),
            SameItemSwitchKind::AudioLanguage {
                parent_generation: action.parent_generation(),
                purpose,
            },
            playlist_runtime,
            renderer,
        )
    }
}
