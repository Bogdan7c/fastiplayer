//! Общие помощники интеграционных тестов: загрузка и запрос «что видно в момент T».

use subtitle_core::{SubtitleContent, SubtitleEvent, SubtitleTime, SubtitleTimeline};
use subtitle_formats::{
    LegacyEncodingHint, LoadedTextSubtitle, SubtitleLoadError, SubtitleParseLimits,
    TextSubtitleFormat, load_text_subtitle,
};

/// Щедрые лимиты: тесты, проверяющие лимиты, задают свои.
pub const ROOMY_LIMITS: SubtitleParseLimits = SubtitleParseLimits::new(1 << 20, 10_000, 100);

/// Загружает байты как указанный текстовый формат.
pub fn try_load_text(
    format: TextSubtitleFormat,
    bytes: &[u8],
    hint: LegacyEncodingHint,
    limits: &SubtitleParseLimits,
) -> Result<LoadedTextSubtitle, SubtitleLoadError> {
    load_text_subtitle(bytes, format, hint, limits)
}

/// Тексты реплик, активных в момент `millis` (после сборки реального `SubtitleTimeline`).
pub fn active_texts_at(loaded: &LoadedTextSubtitle, millis: i64) -> Vec<String> {
    let (timeline, _) = SubtitleTimeline::from_events(loaded.document().events().iter().cloned());
    let mut ids = Vec::new();
    timeline.collect_active_at(SubtitleTime::from_millis(millis), &mut ids);
    ids.into_iter()
        .map(|id| plain_text_of(timeline.event(id).unwrap()))
        .collect()
}

pub fn plain_text_of(event: &SubtitleEvent) -> String {
    match event.content() {
        SubtitleContent::Text(cue) => cue.text.plain_text(),
        _ => panic!("unexpected non-text subtitle content"),
    }
}
