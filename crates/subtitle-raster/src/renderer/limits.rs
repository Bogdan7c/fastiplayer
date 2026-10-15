//! Лимиты растеризатора: кэши и вход.
//!
//! Значений по умолчанию нет: их задаёт конфиг (сессия 04). Лимиты входа защищают растеризатор
//! от чрезмерных данных (враждебная дорожка, ошибка в вызывающем коде): работа и память на
//! кадр ограничены сверху независимо от того, что пришло в запросе.

use subtitle_core::{SpanContent, StyledText};

use super::request::RasterCacheLimits;

/// Лимиты на входные данные одного кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterInputLimits {
    /// Максимум символов в одной реплике (переводы строк считаются символами).
    /// Реплика длиннее пропускается целиком (счётчик `rejected_cues_too_long`).
    pub max_chars_per_cue: usize,
    /// Максимум реплик в одном кадре. Рисуются первые по порядку запроса, остальные
    /// отбрасываются (счётчик `dropped_cues_over_frame_limit`).
    pub max_cues_per_frame: usize,
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
}
