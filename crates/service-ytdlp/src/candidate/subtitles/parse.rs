//! Терпимый разбор `subtitles` / `automatic_captions` в инвентарь дорожек.

use std::collections::BTreeMap;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;
use source_core::HttpRequestTarget;
use url::Url;
use web_media_core::{
    LanguageTag, MAX_SUBTITLE_DESCRIPTORS, SemanticIdentity, SourceIdentity, SubtitleDescriptor,
    SubtitleDisplayName, SubtitleFormatIdentity, SubtitleKind,
};

use super::{YtDlpSubtitleFormat, YtDlpSubtitleInventory, YtDlpSubtitleSource, YtDlpSubtitleTrack};

/// Суффикс ключа исходного языка в `automatic_captions` (`en-orig`).
const ORIGINAL_LANGUAGE_SUFFIX: &str = "-orig";
/// Протокол чата прямой трансляции: это не субтитры.
const LIVE_CHAT_MARKER: &str = "live_chat";

/// Собирает инвентарь из двух карт yt-dlp. Любая кривая запись пропускается.
pub(in crate::candidate) fn normalize_subtitle_inventory(
    manual: Option<&Value>,
    automatic: Option<&Value>,
    source: SourceIdentity,
) -> YtDlpSubtitleInventory {
    let mut collector = TrackCollector::default();
    collect_map(manual, MapOrigin::Manual, &mut collector);
    collect_map(automatic, MapOrigin::Automatic, &mut collector);
    collector.finish(source)
}

/// Из какой карты yt-dlp пришли записи.
#[derive(Clone, Copy)]
enum MapOrigin {
    /// `subtitles`.
    Manual,
    /// `automatic_captions`.
    Automatic,
}

/// Ключ дорожки: вид + язык. Одинаковый ключ означает одну дорожку.
type TrackKey = (SubtitleKind, String);

/// Накопленные данные одной дорожки до сборки дескриптора.
#[derive(Default)]
struct TrackDraft {
    /// Название от yt-dlp (первое непустое).
    name: Option<String>,
    /// Ссылки по форматам; для каждого формата берётся первая ссылка.
    sources: BTreeMap<YtDlpSubtitleFormat, YtDlpSubtitleSource>,
}

/// Накопитель с детерминированным порядком (по виду, затем по языку).
#[derive(Default)]
struct TrackCollector {
    /// Черновики дорожек.
    drafts: BTreeMap<TrackKey, TrackDraft>,
}

fn collect_map(map: Option<&Value>, origin: MapOrigin, collector: &mut TrackCollector) {
    let Some(Value::Object(languages)) = map else {
        return;
    };
    // Сначала обычные ключи, потом `-orig`: у дубликата ASR побеждает чистое имя.
    let (original_keys, plain_keys): (Vec<_>, Vec<_>) = languages
        .iter()
        .partition(|(key, _)| key.ends_with(ORIGINAL_LANGUAGE_SUFFIX));
    for (language_key, entries) in plain_keys.into_iter().chain(original_keys) {
        if language_key == LIVE_CHAT_MARKER {
            continue;
        }
        let language = language_key
            .strip_suffix(ORIGINAL_LANGUAGE_SUFFIX)
            .unwrap_or(language_key);
        let Value::Array(entries) = entries else {
            continue;
        };
        for entry in entries {
            collect_entry(entry, language, origin, collector);
        }
    }
}

fn collect_entry(entry: &Value, language: &str, origin: MapOrigin, collector: &mut TrackCollector) {
    let Value::Object(fields) = entry else {
        return;
    };
    // Скачать можно только обычный HTTP(S)-файл. Чат трансляции (`youtube_live_chat`)
    // и HLS-плейлисты субтитров (`m3u8_native`, их YouTube кладёт рядом с каждой
    // дорожкой в `automatic_captions`) — это не файлы субтитров.
    if fields
        .get("protocol")
        .and_then(Value::as_str)
        .is_some_and(|protocol| !matches!(protocol, "http" | "https"))
    {
        return;
    }
    let Some(format) = fields
        .get("ext")
        .and_then(Value::as_str)
        .and_then(YtDlpSubtitleFormat::from_extension)
    else {
        return;
    };
    let Some(url_text) = fields.get("url").and_then(Value::as_str) else {
        return;
    };
    let Ok(parsed_url) = Url::parse(url_text) else {
        return;
    };
    let Ok(target) = HttpRequestTarget::parse_exact(url_text) else {
        return;
    };
    let kind = classify_kind(origin, &parsed_url);
    // Язык автоперевода берём из `tlang`, если он есть: ключ карты бывает составным.
    let language = match (kind, query_value(&parsed_url, "tlang")) {
        (SubtitleKind::AutoTranslated, Some(target_language)) if !target_language.is_empty() => {
            target_language
        }
        _ => language.to_owned(),
    };
    if language.is_empty() {
        return;
    }
    let draft = collector.drafts.entry((kind, language)).or_default();
    if draft.name.is_none() {
        draft.name = fields
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
    }
    let expires_at = query_value(&parsed_url, "expire")
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)));
    draft
        .sources
        .entry(format)
        .or_insert_with(|| YtDlpSubtitleSource::new(format, target, expires_at));
}

/// Определяет вид по карте и параметрам ссылки (`kind=asr`, `tlang`).
fn classify_kind(origin: MapOrigin, url: &Url) -> SubtitleKind {
    match origin {
        MapOrigin::Manual => SubtitleKind::Manual,
        MapOrigin::Automatic if query_value(url, "tlang").is_some() => SubtitleKind::AutoTranslated,
        // Распознанная речь: `kind=asr` без `tlang`; чужие экстракторы без пометок
        // тоже считаем распознанной речью, раз они лежат в automatic_captions.
        MapOrigin::Automatic => SubtitleKind::AutoGenerated,
    }
}

/// Значение первого query-параметра с таким именем.
fn query_value(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

impl TrackCollector {
    /// Строит дескрипторы, отбрасывает дорожки без нужных форматов и режет по лимиту.
    fn finish(self, source: SourceIdentity) -> YtDlpSubtitleInventory {
        // BTreeMap уже отсортирован: Manual < AutoGenerated < AutoTranslated, затем язык.
        let mut tracks: Vec<YtDlpSubtitleTrack> = self
            .drafts
            .into_iter()
            .filter_map(|((kind, language), draft)| build_track(source, kind, &language, draft))
            .collect();
        let omitted = tracks.len().saturating_sub(MAX_SUBTITLE_DESCRIPTORS);
        tracks.truncate(MAX_SUBTITLE_DESCRIPTORS);
        YtDlpSubtitleInventory::new(tracks, omitted)
    }
}

fn build_track(
    source: SourceIdentity,
    kind: SubtitleKind,
    language: &str,
    draft: TrackDraft,
) -> Option<YtDlpSubtitleTrack> {
    // BTreeMap по формату даёт порядок Srv3, WebVtt — то есть порядок предпочтения.
    let sources: Vec<YtDlpSubtitleSource> = draft.sources.into_values().collect();
    if sources.is_empty() {
        return None;
    }
    let formats = sources
        .iter()
        .map(|source| SubtitleFormatIdentity::new(source.format().extension()))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let identity = SemanticIdentity::new(
        source,
        format!("yt-dlp-subtitle-v1-{}-{language}", kind.stable_key()),
    )
    .ok()?;
    let mut descriptor = SubtitleDescriptor::new(identity, kind, formats)
        .ok()?
        .with_language(LanguageTag::new(language).ok()?);
    if let Some(name) = draft.name.and_then(bounded_display_name) {
        descriptor = descriptor.with_display_name(name);
    }
    Some(YtDlpSubtitleTrack::new(descriptor, sources))
}

/// Обрезает слишком длинное название по границе символа.
fn bounded_display_name(name: String) -> Option<SubtitleDisplayName> {
    const MAX_BYTES: usize = 128;
    let mut end = name.len().min(MAX_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    SubtitleDisplayName::new(&name[..end]).ok()
}
