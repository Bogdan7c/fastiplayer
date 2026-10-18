//! Extractor-private reconstructible selection state за neutral app boundary.

use std::sync::Arc;

use service_ytdlp::YtDlpCandidateSelection;

use super::audio_languages::{
    ExtractorAudioLanguageCatalog, WebAudioLanguageAction, WebAudioLanguageChoice,
    WebAudioLanguageResolution,
};
use super::{YtDlpCandidateOpenIntent, catalog};

/// Provider DTO и reverse routes никогда не пересекают этот adapter owner.
#[derive(Clone, PartialEq, Eq)]
pub struct ExtractorMediaSourceState {
    pub(super) neutral_selection: web_media_core::WebMediaSelection,
    pub(super) candidate_selection: YtDlpCandidateSelection,
    pub(super) composed_selection: Option<Box<service_ytdlp::YtDlpComposedSelection>>,
    pub(super) stream_configuration: crate::web_media_stream_model::WebMediaStreamConfiguration,
    pub(super) catalog_attachment: crate::web_media_catalog::WebMediaCatalogAttachment,
    pub(super) catalog_selection_routes: Arc<[catalog::ExtractorCatalogSelectionRoute]>,
    /// Языки звука (дубляжи) раздельной пары «видео + аудио»; пусто, если выбора нет.
    pub(super) audio_languages: ExtractorAudioLanguageCatalog,
    /// Загрузка субтитров видео; секретные ссылки живут только внутри дескриптора.
    pub(super) subtitle_fetch: crate::web_subtitle_fetch::WebSubtitleFetchHandle,
}

impl ExtractorMediaSourceState {
    /// Возвращает canonical N01 selection без provider token-а.
    pub const fn neutral_selection(&self) -> &web_media_core::WebMediaSelection {
        &self.neutral_selection
    }

    /// Возвращает secret-safe installed stream projection для UI model owner-а.
    pub const fn stream_configuration(
        &self,
    ) -> &crate::web_media_stream_model::WebMediaStreamConfiguration {
        &self.stream_configuration
    }

    /// Дескриптор загрузки субтитров этого видео (клонируется дёшево, `Send`).
    ///
    /// Список дорожек без секретов: `stream_configuration().web_subtitle_tracks()`.
    pub fn web_subtitle_fetch_handle(&self) -> crate::web_subtitle_fetch::WebSubtitleFetchHandle {
        self.subtitle_fetch.clone()
    }

    /// Языки звука, между которыми можно переключиться без смены видео.
    ///
    /// Пусто: у ролика один язык, активный кандидат склеен (muxed) либо источник не
    /// публикует языки. Секретов в ответе нет.
    #[must_use]
    pub fn audio_language_choices(&self) -> Vec<WebAudioLanguageChoice> {
        self.audio_languages
            .choices(self.stream_configuration.generation())
    }

    /// Разбирает выбор языка и строит переоткрытие того же ролика с другой дорожкой.
    #[must_use]
    pub fn audio_language_switch_intent(
        &self,
        action: WebAudioLanguageAction,
    ) -> WebAudioLanguageResolution {
        self.audio_languages.resolve(
            action,
            self.stream_configuration.generation(),
            self.stream_configuration.preference(),
        )
    }

    /// Возвращает neutral catalog attachment без extractor route payload-а.
    pub const fn catalog_attachment(&self) -> &crate::web_media_catalog::WebMediaCatalogAttachment {
        &self.catalog_attachment
    }

    /// Разрешает neutral catalog target внутри extractor adapter-а.
    pub fn selection_intent_for_target(
        &self,
        target: &crate::web_media_catalog::WebMediaSelectionTarget,
    ) -> Option<YtDlpCandidateOpenIntent> {
        self.catalog_selection_routes
            .iter()
            .find(|route| route.target() == target)
            .map(|route| route.selection_intent(self.stream_configuration.preference()))
    }

    /// Строит component reopen, не раскрывая provider parent token lifecycle-у.
    pub fn selection_intent_for_component(
        &self,
        semantic_selection: web_media_core::ComponentVariantSemanticSelectionRequest,
    ) -> YtDlpCandidateOpenIntent {
        YtDlpCandidateOpenIntent::exact_with_component_semantic_selection(
            Box::new(self.candidate_selection.clone()),
            &self.stream_configuration,
            semantic_selection,
        )
    }

    /// Сохраняет установленный parent/component выбор при suspend/reopen.
    pub fn installed_reopen_intent(&self) -> YtDlpCandidateOpenIntent {
        match &self.composed_selection {
            Some(selection) => YtDlpCandidateOpenIntent::composed(
                selection.clone(),
                Box::new(self.candidate_selection.clone()),
                self.stream_configuration.preference(),
            ),
            None => YtDlpCandidateOpenIntent::exact_preserving_installed_stream_configuration(
                Box::new(self.candidate_selection.clone()),
                &self.stream_configuration,
            ),
        }
    }

    /// Extractor-focused tests проверяют exact rematch token только внутри adapter module.
    /// Открыт тестам `app-egui` через feature `test-fixtures` (в рабочую сборку не попадает).
    #[cfg(any(test, feature = "test-fixtures"))]
    pub const fn candidate_selection(&self) -> &YtDlpCandidateSelection {
        &self.candidate_selection
    }
}
