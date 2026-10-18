//! Языки звука (дубляжи) yt-dlp-видео с раздельными дорожками «видео + аудио».
//!
//! # Владение
//! Каталог языков строится один раз при подготовке источника (`prepare_yt_dlp_web_media`,
//! пока жив снимок кандидатов) и живёт в `ExtractorMediaSourceState`. Наружу выходят только
//! нейтральные [`WebAudioLanguageChoice`] (язык, «оригинал», «играет») и непрозрачное
//! действие [`WebAudioLanguageAction`]; провайдерские выборы (`YtDlpComposedSelection`)
//! приложение не видит.
//!
//! # Правила
//! - Для каждого языка берётся лучший по обычному fallback-рангу audio-only кандидат,
//!   который `compose_inventory_av` принимает вместе с АКТИВНЫМ видео. Видео при смене языка
//!   не меняется.
//! - «Оригинал» — язык с наибольшим `language_preference`, если оно у него строго больше,
//!   чем хотя бы у одного другого языка (так YouTube отличает оригинал от дубляжей). Если
//!   предпочтения не объявлены или равны — оригинал не помечается (угадывать нельзя).
//! - Меньше двух языков, не-раздельный (muxed) активный кандидат или активный язык не из
//!   списка — выбора нет, каталог пуст.
//! - Действие привязано к поколению конфигурации потока: после переоткрытия старое действие
//!   даёт [`WebAudioLanguageResolution::Stale`].

use std::sync::Arc;

use service_ytdlp::{
    YtDlpCandidateSelection, YtDlpCandidateSnapshot, YtDlpComposedSelection,
    YtDlpNormalizedCandidate,
};
use web_media_core::{CandidateDescriptor, LanguageTag, StreamLayout};

use super::YtDlpCandidateOpenIntent;
use super::catalog::{compose_catalog_inventory_av, ranked_playable_audio};
use crate::web_media_stream_model::{WebMediaSelectionPreference, WebMediaStreamGeneration};
use web_media_playback_plan::{
    PlanningCandidateSnapshot, PlaybackCapabilitySnapshot, PlaybackSelectionPolicy,
};

/// Сколько языков нужно, чтобы выбор имел смысл.
const MIN_LANGUAGES_TO_SWITCH: usize = 2;

/// Непрозрачное действие «переключить язык звука», привязанное к поколению потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebAudioLanguageAction {
    /// Поколение конфигурации, из которой действие получено.
    parent_generation: WebMediaStreamGeneration,
    /// Индекс маршрута в каталоге языков того же поколения.
    route_index: usize,
}

impl WebAudioLanguageAction {
    /// Строит действие только для hermetic UI/state тестов (вне crate — с `test-fixtures`).
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub const fn for_test(parent_generation: WebMediaStreamGeneration, route_index: usize) -> Self {
        Self {
            parent_generation,
            route_index,
        }
    }

    /// Поколение установленного потока, которому принадлежит действие.
    #[must_use]
    pub const fn parent_generation(&self) -> WebMediaStreamGeneration {
        self.parent_generation
    }
}

/// Один язык звука, готовый к показу в меню.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAudioLanguageChoice {
    /// Что выполнить при выборе.
    pub action: WebAudioLanguageAction,
    /// Тег языка из метаданных формата (`en-US`, `ru`).
    pub language: LanguageTag,
    /// Это оригинальная дорожка ролика.
    pub is_original: bool,
    /// Этот язык сейчас установлен.
    pub is_active: bool,
}

/// Результат разбора действия владельцем каталога.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebAudioLanguageResolution {
    /// Язык уже играет, переоткрывать нечего.
    NoChange,
    /// Действие из другого поколения (источник переоткрылся) либо с неизвестным индексом.
    Stale,
    /// Открыть тот же ролик с другой аудиодорожкой.
    Ready(YtDlpCandidateOpenIntent),
}

/// Приватный маршрут: язык + готовая композиция «активное видео + звук этого языка».
#[derive(Clone, PartialEq, Eq)]
struct ExtractorAudioLanguageRoute {
    language: LanguageTag,
    is_original: bool,
    selection: Box<YtDlpComposedSelection>,
}

/// Каталог языков звука одного установленного источника.
#[derive(Clone, PartialEq, Eq, Default)]
pub(super) struct ExtractorAudioLanguageCatalog {
    routes: Arc<[ExtractorAudioLanguageRoute]>,
    /// Индекс маршрута активного языка; всегда `Some`, если маршруты не пусты.
    active_index: Option<usize>,
    /// Video-only строка inventory активной пары: при смене языка видео не меняется.
    active_video: Option<Box<YtDlpCandidateSelection>>,
}

/// Вход построения каталога: всё, что известно к моменту подготовки источника.
pub(super) struct AudioLanguageCatalogRequest<'a> {
    pub(super) snapshot: &'a YtDlpCandidateSnapshot,
    /// Playable audio-only кандидаты в порядке fallback-ранга (`catalog::ranked_playable_audio`).
    pub(super) ranked_audio: &'a [&'a YtDlpNormalizedCandidate],
    /// Video-only строка inventory активной пары (parent раздельной пары).
    pub(super) active_video: &'a YtDlpCandidateSelection,
    /// Описание установленной раздельной пары «видео + звук».
    pub(super) active_descriptor: &'a CandidateDescriptor,
}

impl ExtractorAudioLanguageCatalog {
    /// Строит каталог для только что подготовленного источника.
    ///
    /// Сам считает планировщиковый ранг, чтобы отбросить неиграбельные форматы тем же
    /// правилом, что и каталог качества (`catalog::ranked_playable_audio`).
    pub(super) fn for_prepared_source(
        snapshot: &YtDlpCandidateSnapshot,
        planning: &PlanningCandidateSnapshot,
        capabilities: PlaybackCapabilitySnapshot<'_>,
        policy: &PlaybackSelectionPolicy,
        active_selection: &YtDlpCandidateSelection,
        active_composed: Option<&YtDlpComposedSelection>,
    ) -> anyhow::Result<Self> {
        let Some(active_pair) = active_pair(snapshot, active_selection, active_composed) else {
            // Muxed-кандидат либо compound без video-only строки в inventory.
            return Ok(Self::default());
        };
        let ranking = web_media_playback_plan::rank_playable_opaque_alternatives(
            planning,
            capabilities,
            policy,
        )?;
        let rejected = ranking
            .rejected_candidates()
            .iter()
            .map(|candidate| candidate.exact_identity())
            .collect::<std::collections::HashSet<_>>();
        let ranked_audio = ranked_playable_audio(snapshot, &rejected);
        Ok(Self::build(&AudioLanguageCatalogRequest {
            snapshot,
            ranked_audio: &ranked_audio,
            active_video: &active_pair.video,
            active_descriptor: &active_pair.descriptor,
        }))
    }

    /// Строит каталог; при отсутствии выбора возвращает пустой.
    pub(super) fn build(request: &AudioLanguageCatalogRequest<'_>) -> Self {
        let Some(active_language) = composed_audio_language(request.active_descriptor) else {
            return Self::default();
        };
        let mut best_per_language = Vec::<LanguageRouteDraft>::new();
        for audio in request.ranked_audio {
            let Some(language) = candidate_audio_language(audio.descriptor()) else {
                continue;
            };
            if best_per_language
                .iter()
                .any(|draft| &draft.language == language)
            {
                continue;
            }
            if let Some(selection) = compose_with_active_video(request, audio) {
                best_per_language.push(LanguageRouteDraft {
                    language: language.clone(),
                    language_preference: audio.declared_language_preference(),
                    selection,
                });
            }
        }
        if best_per_language.len() < MIN_LANGUAGES_TO_SWITCH {
            return Self::default();
        }
        let Some(active_index) = best_per_language
            .iter()
            .position(|draft| &draft.language == active_language)
        else {
            // Установленный язык не среди предложенных: меню показало бы пустую отметку.
            return Self::default();
        };
        let original_language = original_language(&best_per_language);
        let routes: Arc<[ExtractorAudioLanguageRoute]> = best_per_language
            .into_iter()
            .map(|draft| ExtractorAudioLanguageRoute {
                is_original: original_language.as_ref() == Some(&draft.language),
                language: draft.language,
                selection: Box::new(draft.selection),
            })
            .collect();
        Self {
            routes,
            active_index: Some(active_index),
            active_video: Some(Box::new(request.active_video.clone())),
        }
    }

    /// Нейтральный список языков; пустой — выбора нет.
    pub(super) fn choices(
        &self,
        generation: WebMediaStreamGeneration,
    ) -> Vec<WebAudioLanguageChoice> {
        self.routes
            .iter()
            .enumerate()
            .map(|(route_index, route)| WebAudioLanguageChoice {
                action: WebAudioLanguageAction {
                    parent_generation: generation,
                    route_index,
                },
                language: route.language.clone(),
                is_original: route.is_original,
                is_active: self.active_index == Some(route_index),
            })
            .collect()
    }

    /// Разбирает действие и, если нужно, строит open-intent для смены языка.
    pub(super) fn resolve(
        &self,
        action: WebAudioLanguageAction,
        current_generation: WebMediaStreamGeneration,
        preference: WebMediaSelectionPreference,
    ) -> WebAudioLanguageResolution {
        if action.parent_generation != current_generation {
            return WebAudioLanguageResolution::Stale;
        }
        let Some(route) = self.routes.get(action.route_index) else {
            return WebAudioLanguageResolution::Stale;
        };
        if self.active_index == Some(action.route_index) {
            return WebAudioLanguageResolution::NoChange;
        }
        let Some(active_video) = &self.active_video else {
            return WebAudioLanguageResolution::Stale;
        };
        WebAudioLanguageResolution::Ready(YtDlpCandidateOpenIntent::composed(
            route.selection.clone(),
            active_video.clone(),
            preference,
        ))
    }
}

/// Черновик маршрута до того, как известен оригинал.
struct LanguageRouteDraft {
    language: LanguageTag,
    language_preference: Option<i64>,
    selection: YtDlpComposedSelection,
}

/// Композиция «активное видео + этот звук»; несовместимая пара — просто не предлагается.
fn compose_with_active_video(
    request: &AudioLanguageCatalogRequest<'_>,
    audio: &YtDlpNormalizedCandidate,
) -> Option<YtDlpComposedSelection> {
    let audio_selection = match request.snapshot.selection_for(audio) {
        Ok(selection) => selection,
        Err(error) => {
            tracing::warn!(%error, "Audio language skipped: candidate has no exact selection");
            return None;
        }
    };
    match compose_catalog_inventory_av(request.snapshot, request.active_video, &audio_selection) {
        Ok(composed) => composed,
        Err(error) => {
            tracing::warn!(%error, "Audio language skipped: pair did not compose");
            None
        }
    }
}

/// Язык оригинала по `language_preference`: строго выше хотя бы одного другого языка.
fn original_language(drafts: &[LanguageRouteDraft]) -> Option<LanguageTag> {
    let best = drafts
        .iter()
        .max_by_key(|draft| draft.language_preference)?;
    let best_preference = best.language_preference?;
    let is_unique_best = drafts
        .iter()
        .filter(|draft| draft.language_preference == Some(best_preference))
        .count()
        == 1;
    let beats_someone = drafts.iter().any(|draft| {
        draft
            .language_preference
            .is_none_or(|value| value < best_preference)
    });
    (is_unique_best && beats_someone).then(|| best.language.clone())
}

/// Активная пара: video-only строка inventory и описание пары.
struct ActivePair {
    video: YtDlpCandidateSelection,
    descriptor: CandidateDescriptor,
}

/// Определяет активную пару для двух форм установленного источника.
///
/// - Раздельная пара из каталога качества: `selection` — её video-only parent.
/// - Compound по умолчанию (`requested_formats` yt-dlp, так открывается YouTube):
///   video-only строку ищет владелец снимка по тому же request material.
fn active_pair(
    snapshot: &YtDlpCandidateSnapshot,
    active_selection: &YtDlpCandidateSelection,
    active_composed: Option<&YtDlpComposedSelection>,
) -> Option<ActivePair> {
    if let Some(composed) = active_composed {
        return Some(ActivePair {
            video: active_selection.clone(),
            descriptor: composed.descriptor().clone(),
        });
    }
    let candidate = snapshot.canonical_candidate_for_planning_identity(
        active_selection.exact_identity(),
        active_selection.semantic_identity(),
    )?;
    if !matches!(
        candidate.descriptor().layout(),
        StreamLayout::Separate { .. }
    ) {
        return None;
    }
    Some(ActivePair {
        video: snapshot.inventory_video_of_compound(active_selection)?,
        descriptor: candidate.descriptor().clone(),
    })
}

/// Язык audio-only кандидата.
fn candidate_audio_language(descriptor: &CandidateDescriptor) -> Option<&LanguageTag> {
    match descriptor.layout() {
        StreamLayout::AudioOnly(audio) => audio.audio().language(),
        _ => None,
    }
}

/// Язык звука установленной раздельной пары.
fn composed_audio_language(descriptor: &CandidateDescriptor) -> Option<&LanguageTag> {
    match descriptor.layout() {
        StreamLayout::Separate { audio, .. } => audio.audio().language(),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
