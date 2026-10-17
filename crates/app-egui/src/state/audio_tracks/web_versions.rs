//! Web-версии звука (HLS `EXT-X-MEDIA TYPE=AUDIO`, DASH audio Representation) для меню «♪».
//!
//! # Владение
//! - Каталог вариантов и активная строка принадлежат `media-source-open`
//!   (`WebMediaStreamConfiguration::component_variant_projection`). Здесь только read-only
//!   проекция для меню: подпись + безопасное действие `ComponentVariantSelectionAction`.
//! - Само переключение — существующий same-item component switch
//!   (`state/same_item_candidate_switch.rs`): он сохраняет позицию и Play/Pause, старое
//!   воспроизведение живёт до успешной установки новой версии.
//!
//! # Когда версии попадают в меню
//! Только если у источника есть независимая audio axis (`VideoAndAudio` или `AudioOnly`) и в
//! ней хотя бы две строки. Coupled axis (звук «склеен» с видео в одном варианте) сюда не
//! попадает: её выбор меняет и картинку, это работа панели качества, а не меню звука.
//!
//! # Подпись
//! `<язык> (<кодек> <раскладка>)`, например «English (E-AC-3 5.1)». Без языка — «Audio
//! version N (AAC 2.0)». Если две строки получили одинаковую подпись, к ним добавляется
//! битрейт («…, 128 kbps»), чтобы пользователь их различал.

use std::collections::HashMap;

use crate::ui::url_sidebar::codec_label;
use crate::web_media_stream_model::WebMediaStreamGeneration;
use crate::web_media_stream_model::component_variants::{
    ComponentVariantSelectionAction, WebMediaAudioComponentVariantAxis,
    WebMediaAudioComponentVariantPresentation, WebMediaComponentVariantAxisKind,
    WebMediaComponentVariantProjection, WebMediaInstalledComponentVariantPresentation,
};
use web_media_core::ComponentVariantCatalogGeneration;

use super::labels::language_display_name;

/// Сколько web-версий нужно, чтобы выбор имел смысл.
const MIN_VERSIONS_TO_SWITCH: usize = 2;
/// Делитель bit/s → kbit/s для подписи битрейта.
const BITS_PER_KILOBIT: u64 = 1_000;

/// Одна web-версия звука, готовая к показу в меню.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WebAudioVersionEntry {
    /// Безопасное действие с двумя generation-fence-ами; проверяет его владелец каталога.
    pub(super) action: ComponentVariantSelectionAction,
    /// Подпись: «English (E-AC-3 5.1)».
    pub(super) label: String,
    /// Эта версия сейчас установлена.
    pub(super) is_active: bool,
}

/// Строит список web-версий; `None` — источник не публикует независимую audio axis с выбором.
pub(super) fn web_audio_versions(
    parent_generation: WebMediaStreamGeneration,
    projection: &WebMediaComponentVariantProjection,
) -> Option<Vec<WebAudioVersionEntry>> {
    let (catalog_generation, axis) = independent_audio_axis(projection)?;
    if axis.variants.len() < MIN_VERSIONS_TO_SWITCH {
        return None;
    }
    let base_labels: Vec<String> = axis
        .variants
        .iter()
        .enumerate()
        .map(|(variant_index, variant)| version_label(variant, variant_index, BitrateDetail::Omit))
        .collect();
    let duplicate_counts = count_labels(&base_labels);
    let entries = axis
        .variants
        .iter()
        .zip(base_labels)
        .enumerate()
        .map(|(variant_index, (variant, base_label))| {
            // Совпавшие подписи уточняются битрейтом; уникальные остаются короткими.
            let label = if duplicate_counts.get(&base_label).copied().unwrap_or(0) > 1 {
                version_label(variant, variant_index, BitrateDetail::Include)
            } else {
                base_label
            };
            WebAudioVersionEntry {
                action: ComponentVariantSelectionAction {
                    parent_generation,
                    catalog_generation,
                    axis: WebMediaComponentVariantAxisKind::Audio,
                    variant_index,
                },
                label,
                is_active: variant_index == axis.active_index,
            }
        })
        .collect();
    Some(entries)
}

/// Возвращает независимую audio axis установленного каталога (без coupled A/V строк).
fn independent_audio_axis(
    projection: &WebMediaComponentVariantProjection,
) -> Option<(
    ComponentVariantCatalogGeneration,
    &WebMediaAudioComponentVariantAxis,
)> {
    let WebMediaComponentVariantProjection::Installed(installed) = projection else {
        return None;
    };
    match installed {
        WebMediaInstalledComponentVariantPresentation::VideoAndAudio {
            catalog_generation,
            audio,
            ..
        }
        | WebMediaInstalledComponentVariantPresentation::AudioOnly {
            catalog_generation,
            audio,
        } => Some((*catalog_generation, audio)),
        WebMediaInstalledComponentVariantPresentation::VideoOnly { .. }
        | WebMediaInstalledComponentVariantPresentation::Coupled { .. } => None,
    }
}

/// Нужно ли уточнять подпись битрейтом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BitrateDetail {
    /// Обычная короткая подпись.
    Omit,
    /// Подпись совпала с соседней: добавляем битрейт, если он объявлен.
    Include,
}

/// Подпись одной версии: язык (или «Audio version N») + кодек, раскладка и, при нужде, битрейт.
fn version_label(
    variant: &WebMediaAudioComponentVariantPresentation,
    variant_index: usize,
    bitrate_detail: BitrateDetail,
) -> String {
    let head = variant
        .language_label
        .as_deref()
        .and_then(language_display_name)
        .unwrap_or_else(|| format!("Audio version {}", variant_index + 1));
    let format_details: Vec<String> = [
        variant.codec.map(|codec| codec_label(codec).to_owned()),
        variant.channels.map(channel_layout_label),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut details = Vec::new();
    if !format_details.is_empty() {
        details.push(format_details.join(" "));
    }
    if bitrate_detail == BitrateDetail::Include
        && let Some(bitrate) = variant.bitrate
    {
        details.push(format!("{} kbps", bitrate / BITS_PER_KILOBIT));
    }
    if details.is_empty() {
        head
    } else {
        format!("{head} ({})", details.join(", "))
    }
}

/// Привычное имя раскладки по числу каналов: 2 → «2.0», 6 → «5.1», 8 → «7.1».
///
/// Каталог публикует только число каналов (без маски позиций), поэтому для прочих чисел
/// честно пишем «N ch», а не угадываем раскладку.
pub(super) fn channel_layout_label(channels: u16) -> String {
    match channels {
        1 => "Mono".to_owned(),
        2 => "2.0".to_owned(),
        6 => "5.1".to_owned(),
        8 => "7.1".to_owned(),
        other => format!("{other} ch"),
    }
}

/// Сколько раз встречается каждая подпись (для уточнения дубликатов).
fn count_labels(labels: &[String]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for label in labels {
        *counts.entry(label.clone()).or_insert(0) += 1;
    }
    counts
}
