//! Лимиты растеризатора: кэши и вход.
//!
//! Значений по умолчанию нет: их задаёт конфиг (сессия 04). Лимиты входа защищают растеризатор
//! от чрезмерных данных (враждебная дорожка, ошибка в вызывающем коде): работа и память на
//! кадр ограничены сверху независимо от того, что пришло в запросе.

use subtitle_core::{AssTextItem, SpanContent, StyledText};

use super::request::{CueContent, RasterCacheLimits, RenderableCue};

/// Лимиты на входные данные одного кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterInputLimits {
    /// Максимум символов в одной реплике (переводы строк считаются символами).
    /// Реплика длиннее пропускается целиком (счётчик `rejected_cues_too_long`).
    pub max_chars_per_cue: usize,
    /// Максимум реплик простых форматов (SRT, WebVTT) в одном кадре. Рисуются первые по порядку
    /// запроса, остальные отбрасываются (счётчик `dropped_cues_over_frame_limit`).
    pub max_cues_per_frame: usize,
    /// Максимум ASS-событий в одном кадре. Сложная типографика держит на экране много событий
    /// сразу, поэтому лимит отдельный и выше. Остаются события с наименьшими `(слой, ReadOrder)`,
    /// остальные отбрасываются (счётчик `dropped_ass_events_over_frame_limit`).
    pub max_ass_events_per_frame: usize,
    /// Максимум стороны холста в пикселях. Холст больше получает пустой результат
    /// (счётчик `rejected_canvas`).
    pub max_canvas_side: u32,
}

/// Все лимиты растеризатора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterLimits {
    /// Лимиты кэшей.
    pub cache: RasterCacheLimits,
    /// Лимиты входа.
    pub input: RasterInputLimits,
}

/// Реплики кадра, прошедшие лимиты числа реплик.
pub(super) struct AdmittedCues<'request, 'content> {
    /// Реплики простых форматов в порядке запроса.
    pub(super) text: Vec<&'request RenderableCue<'content>>,
    /// ASS-события в порядке отрисовки: `(слой, ReadOrder)` (стабильно по порядку запроса).
    pub(super) ass: Vec<&'request RenderableCue<'content>>,
    /// Сколько реплик простых форматов отброшено сверх `max_cues_per_frame`.
    pub(super) dropped_text: usize,
    /// Сколько ASS-событий отброшено сверх `max_ass_events_per_frame`.
    pub(super) dropped_ass: usize,
}

/// Применяет лимиты числа реплик. Простые форматы: первые по порядку запроса. ASS: сначала
/// сортировка по `(слой, ReadOrder)` (так же, как рисует libass), затем отсекается хвост —
/// выживают нижние слои и более ранние события, а не те, что случайно оказались в начале
/// таймлайна.
pub(super) fn admit_cues<'request, 'content>(
    cues: &'request [RenderableCue<'content>],
    limits: &RasterInputLimits,
) -> AdmittedCues<'request, 'content> {
    let (mut text, mut ass): (Vec<&RenderableCue<'content>>, Vec<&RenderableCue<'content>>) = cues
        .iter()
        .partition(|cue| matches!(cue.content, CueContent::Text(_)));
    let dropped_text = text.len().saturating_sub(limits.max_cues_per_frame);
    text.truncate(limits.max_cues_per_frame);
    ass.sort_by_key(|cue| {
        let read_order = match cue.content {
            CueContent::Ass { cue: ass_cue, .. } => ass_cue.read_order,
            CueContent::Text(_) => 0,
        };
        (cue.layer, read_order)
    });
    let dropped_ass = ass.len().saturating_sub(limits.max_ass_events_per_frame);
    ass.truncate(limits.max_ass_events_per_frame);
    AdmittedCues {
        text,
        ass,
        dropped_text,
        dropped_ass,
    }
}

/// Число символов реплики по тем же правилам, что у конвейера: символы текстовых спанов плюс
/// по одному на перевод строки. Считает без аллокаций и без разворачивания текста.
pub(super) fn cue_char_count(text: &StyledText) -> usize {
    text.spans().iter().fold(0_usize, |total, span| {
        let chars = match &span.content {
            SpanContent::Text(fragment) => fragment.chars().count(),
            SpanContent::LineBreak => 1,
        };
        total.saturating_add(chars)
    })
}

/// Число символов реплики ASS: символы текста плюс по одному на перевод строки (теги не
/// считаются: они не превращаются в символы).
pub(super) fn ass_cue_char_count(items: &[AssTextItem]) -> usize {
    items.iter().fold(0_usize, |total, item| {
        let chars = match item {
            AssTextItem::Text(text) => text.chars().count(),
            AssTextItem::HardBreak | AssTextItem::SoftBreak => 1,
            AssTextItem::Overrides(_) => 0,
        };
        total.saturating_add(chars)
    })
}

#[cfg(test)]
mod tests {
    use subtitle_core::{SpanStyle, TextSpan};

    use super::*;

    #[test]
    fn counts_characters_not_bytes_and_line_breaks_as_one() {
        let text = StyledText::new(vec![
            TextSpan::text("Привет", SpanStyle::default()),
            TextSpan::line_break(),
            TextSpan::text("ab", SpanStyle::default()),
        ]);
        assert_eq!(cue_char_count(&text), 6 + 1 + 2);
        assert_eq!(cue_char_count(&StyledText::new(vec![])), 0);
    }

    #[test]
    fn ass_count_ignores_tags_and_counts_breaks() {
        let items = [
            AssTextItem::Overrides(vec![]),
            AssTextItem::Text("Привет".to_owned()),
            AssTextItem::HardBreak,
            AssTextItem::SoftBreak,
            AssTextItem::Text("ab".to_owned()),
        ];
        assert_eq!(ass_cue_char_count(&items), 6 + 1 + 1 + 2);
    }
}
