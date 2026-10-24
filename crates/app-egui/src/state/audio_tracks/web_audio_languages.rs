//! Языки звука (дубляж) yt-dlp-видео для меню «♪» и клавиши `B`.
//!
//! # Владение
//! - Список языков и «какой оригинал / какой играет» принадлежит `media-source-open`
//!   (`WebMediaSourceIntent::web_audio_language_choices`). Здесь только read-only проекция
//!   для меню: подпись + непрозрачное действие.
//! - Переключение — общий same-item switch (`apply_audio_language_action`): позиция и
//!   Play/Pause сохраняются, старое воспроизведение живёт до успешной установки.
//!
//! # Подпись
//! Английское название языка («Russian»); оригинал помечается «English (original)». Порядок:
//! оригинал первым, остальные по алфавиту подписи (без учёта регистра). Если у
//! тега нет названия (пустой, «und»), строка получает запасное имя «Audio language N».

use media_source_open::web_media_open::audio_languages::{
    WebAudioLanguageAction, WebAudioLanguageChoice,
};
use render_shell_api::RenderShell;

use super::labels::language_display_name;
use super::web_audio_language_memory::language_to_remember;
use super::web_version_switch::start_notice;
use crate::playlist_runtime::PlaylistRuntime;
use crate::state::AppState;
use crate::state::language_label_disambiguation::{LanguageNameInput, distinguish_language_names};
use crate::state::same_item_candidate_switch::AudioLanguagePurpose;

/// Сколько языков нужно, чтобы выбор имел смысл.
const MIN_LANGUAGES_TO_SWITCH: usize = 2;
/// Пометка оригинальной дорожки в подписи.
const ORIGINAL_MARKER: &str = "original";

/// Один язык звука, готовый к показу в меню.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WebAudioLanguageEntry {
    /// Непрозрачное действие; поколение проверяет владелец источника.
    pub(super) action: WebAudioLanguageAction,
    /// Подпись: «Russian», «English (original)».
    pub(super) label: String,
    /// Этот язык сейчас играет.
    pub(super) is_active: bool,
}

/// Строит строки меню; `None` — выбирать не из чего (меньше двух языков).
pub(super) fn web_audio_languages(
    choices: Vec<WebAudioLanguageChoice>,
) -> Option<Vec<WebAudioLanguageEntry>> {
    if choices.len() < MIN_LANGUAGES_TO_SWITCH {
        return None;
    }
    // Базовые названия считаются по таблице языков; запасное «Audio language N» уникально само.
    let name_inputs: Vec<LanguageNameInput> = choices
        .iter()
        .enumerate()
        .map(|(index, choice)| LanguageNameInput {
            base_name: language_display_name(choice.language.as_str())
                .unwrap_or_else(|| format!("Audio language {}", index + 1)),
            language_tag: choice.language.as_str().to_owned(),
            // У аудио-форматов yt-dlp нет подробного имени региона: двойники различаются тегом.
            source_name: None,
        })
        .collect();
    // `pt-BR` и `pt-PT` не должны оба называться «Portuguese».
    let names = distinguish_language_names(&name_inputs);
    let mut rows: Vec<(bool, WebAudioLanguageEntry)> = choices
        .into_iter()
        .zip(names)
        .map(|(choice, name)| {
            let entry = WebAudioLanguageEntry {
                action: choice.action,
                label: if choice.is_original {
                    format!("{name} ({ORIGINAL_MARKER})")
                } else {
                    name
                },
                is_active: choice.is_active,
            };
            (choice.is_original, entry)
        })
        .collect();
    // Порядок в меню и в круге `B`: оригинал первым, остальные по алфавиту подписи без учёта
    // регистра. Сортировка устойчивая, поэтому равные подписи сохраняют порядок источника.
    rows.sort_by_key(|(is_original, entry)| (!*is_original, entry.label.to_lowercase()));
    let entries = rows.into_iter().map(|(_, entry)| entry).collect();
    Some(entries)
}

impl AppState {
    /// Языки звука активного источника; `None` — источник не extractor или выбора нет.
    pub(super) fn web_audio_language_entries(&self) -> Option<Vec<WebAudioLanguageEntry>> {
        let intent = self.active_media_source.as_ref()?.web_intent()?;
        web_audio_languages(intent.web_audio_language_choices())
    }

    /// Запускает отложенный выбор языка и сообщает о старте/отказе одной плашкой «Audio: …».
    pub(super) fn apply_requested_web_audio_language(
        &mut self,
        action: WebAudioLanguageAction,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &mut dyn RenderShell,
    ) {
        // Подпись берётся до запуска: после Installed источник получит новое поколение.
        let label = self.web_audio_language_entries().and_then(|entries| {
            entries
                .into_iter()
                .find(|entry| entry.action == action)
                .map(|entry| entry.label)
        });
        // Что запомнить после установки: тег выбранного языка (оригинал тоже).
        let remember_as = self
            .active_media_source
            .as_ref()
            .and_then(|source| source.web_intent())
            .and_then(|intent| language_to_remember(&intent.web_audio_language_choices(), action));
        let outcome = self.apply_audio_language_action(
            action,
            AudioLanguagePurpose::UserChoice { remember_as },
            playlist_runtime,
            renderer,
        );
        if let Err(error) = &outcome {
            tracing::warn!(%error, "Audio language switch was rejected before start");
        }
        if let Some(notice) = start_notice(&outcome, label.as_deref()) {
            self.notify_audio_version(notice);
        }
    }
}
