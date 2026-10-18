//! Языки звука (дубляжи) yt-dlp-источника: neutral доступ для меню «♪» и same-item switch.
//!
//! Отдельный файл, а не часть `source_actions.rs`: языки — самостоятельный вид действия
//! (не catalog target и не component variant), владелец данных — `ExtractorMediaSourceState`.

use media_source_open::web_media_open::audio_languages::{
    WebAudioLanguageAction, WebAudioLanguageChoice, WebAudioLanguageResolution,
};

use super::source_actions::WebMediaSelectionSwitchResolution;
use super::{
    WebMediaOpenRequest, WebMediaOpenSettings, WebMediaSourceAdapter, WebMediaSourceIntent,
};

impl WebMediaSourceIntent {
    /// Языки звука, между которыми можно переключиться без смены видео.
    ///
    /// Пусто для не-extractor источников (HLS/DASH показывают версии звука через
    /// component-варианты), для роликов с одним языком и для склеенных (muxed) форматов.
    #[must_use]
    pub(crate) fn web_audio_language_choices(&self) -> Vec<WebAudioLanguageChoice> {
        match &*self.adapter {
            WebMediaSourceAdapter::Extractor { source_state, .. } => {
                source_state.audio_language_choices()
            }
            _ => Vec::new(),
        }
    }

    /// Разрешает выбор языка в переоткрытие того же ролика с другой аудиодорожкой.
    ///
    /// `Stale` — действие из другого поколения источника; `NoChange` — язык уже играет;
    /// `Unsupported` — источник не extractor и языков не публикует.
    pub(crate) fn audio_language_switch_request(
        &self,
        action: WebAudioLanguageAction,
        settings: WebMediaOpenSettings,
    ) -> WebMediaSelectionSwitchResolution {
        let WebMediaSourceAdapter::Extractor {
            locator,
            source_state,
        } = &*self.adapter
        else {
            return WebMediaSelectionSwitchResolution::Unsupported;
        };
        match source_state.audio_language_switch_intent(action) {
            WebAudioLanguageResolution::NoChange => WebMediaSelectionSwitchResolution::NoChange,
            WebAudioLanguageResolution::Stale => WebMediaSelectionSwitchResolution::Stale,
            WebAudioLanguageResolution::Ready(selection_intent) => {
                WebMediaSelectionSwitchResolution::Ready(WebMediaOpenRequest::extractor(
                    locator.clone(),
                    selection_intent,
                    web_media_core::ExtractorInvocationReason::ExtractorBackedRecovery,
                    settings,
                ))
            }
        }
    }
}
