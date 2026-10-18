//! Событие субтитров: интервал времени + содержимое.

use crate::ass::AssCue;
use crate::picture::SubtitlePictureCue;
use crate::placement::CuePlacement;
use crate::styled_text::StyledText;
use crate::time::{SubtitleTime, SubtitleTimeShift, SubtitleTimeShiftOverflow};

/// Слой наложения: больший слой рисуется поверх меньшего.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubtitleLayer(i32);

impl SubtitleLayer {
    /// Слой по умолчанию.
    pub const DEFAULT: Self = Self(0);

    pub const fn new(value: i32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> i32 {
        self.0
    }
}

/// Текстовая реплика с позиционированием.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleTextCue {
    pub text: StyledText,
    pub placement: CuePlacement,
}

/// Содержимое события.
///
/// Текстовая реплика со стилями автора ([`SubtitleContent::Text`]) или реплика ASS/SSA
/// ([`SubtitleContent::Ass`], сессия 08: стиль и теги хранятся разобранными, а раскладку
/// делает растеризатор) или растровая реплика ([`SubtitleContent::Picture`], сессия 11: PGS,
/// картинка хранится сжатой, распаковывает её crate формата). Будущие виды содержимого
/// добавляются без заглушек заранее, поэтому enum `non_exhaustive`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SubtitleContent {
    Text(SubtitleTextCue),
    Ass(AssCue),
    Picture(SubtitlePictureCue),
}

/// Конец события не позже его начала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "subtitle interval must end after it starts (start {start_micros} us, end {end_micros} us)"
)]
pub struct InvalidSubtitleInterval {
    start_micros: i64,
    end_micros: i64,
}

impl InvalidSubtitleInterval {
    pub fn start(self) -> SubtitleTime {
        SubtitleTime::from_micros(self.start_micros)
    }

    pub fn end(self) -> SubtitleTime {
        SubtitleTime::from_micros(self.end_micros)
    }
}

/// Событие: содержимое, видимое на полуинтервале `[start, end)`.
///
/// Инвариант `end > start` гарантируется конструктором, поэтому поля приватные.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleEvent {
    start: SubtitleTime,
    end: SubtitleTime,
    layer: SubtitleLayer,
    content: SubtitleContent,
}

impl SubtitleEvent {
    pub fn new(
        start: SubtitleTime,
        end: SubtitleTime,
        layer: SubtitleLayer,
        content: SubtitleContent,
    ) -> Result<Self, InvalidSubtitleInterval> {
        if end <= start {
            return Err(InvalidSubtitleInterval {
                start_micros: start.as_micros(),
                end_micros: end.as_micros(),
            });
        }
        Ok(Self {
            start,
            end,
            layer,
            content,
        })
    }

    pub fn start(&self) -> SubtitleTime {
        self.start
    }

    pub fn end(&self) -> SubtitleTime {
        self.end
    }

    pub fn layer(&self) -> SubtitleLayer {
        self.layer
    }

    pub fn content(&self) -> &SubtitleContent {
        &self.content
    }

    /// Копия события, перенесённая на `shift` по времени: сдвигаются начало, конец и моменты
    /// появления слов (`reveal_at`) в тексте; длительность, слой и остальное содержимое те же.
    ///
    /// Остальные виды содержимого абсолютных времён не хранят:
    /// - ASS: `\move`, `\t`, `\fad`, караоке заданы в мс/сс ОТНОСИТЕЛЬНО начала реплики
    ///   (см. `AssMove::start_ms`, `AssKaraoke::centiseconds`), поэтому едут вместе с началом;
    /// - картинка (PGS): только геометрия и палитра.
    ///
    /// Переполнение i64 любого из времён — ошибка (событие не меняется, это `&self`).
    pub fn shifted_by(&self, shift: SubtitleTimeShift) -> Result<Self, SubtitleTimeShiftOverflow> {
        let content = match &self.content {
            SubtitleContent::Text(cue) => SubtitleContent::Text(SubtitleTextCue {
                text: cue.text.shifted_by(shift)?,
                placement: cue.placement.clone(),
            }),
            other => other.clone(),
        };
        // Один и тот же сдвиг сохраняет порядок, поэтому инвариант `end > start` не нарушается.
        Ok(Self {
            start: self.start.shifted_by(shift)?,
            end: self.end.shifted_by(shift)?,
            layer: self.layer,
            content,
        })
    }

    /// Переносит конец раньше. Вызывается только `SubtitleTimeline::shorten_event_end`,
    /// который проверил `start < new_end < end` и обновил индексы; поэтому метод не публичный.
    pub(crate) fn set_shortened_end(&mut self, new_end: SubtitleTime) {
        self.end = new_end;
    }

    /// Полуоткрытый интервал: момент `start` входит, момент `end` уже нет.
    pub fn is_active_at(&self, time: SubtitleTime) -> bool {
        self.start <= time && time < self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styled_text::{SpanStyle, TextSpan};

    fn sample_content() -> SubtitleContent {
        SubtitleContent::Text(SubtitleTextCue {
            text: StyledText::default(),
            placement: CuePlacement::default(),
        })
    }

    fn event(start: i64, end: i64) -> Result<SubtitleEvent, InvalidSubtitleInterval> {
        SubtitleEvent::new(
            SubtitleTime::from_micros(start),
            SubtitleTime::from_micros(end),
            SubtitleLayer::DEFAULT,
            sample_content(),
        )
    }

    #[test]
    fn rejects_end_equal_or_before_start() {
        let equal = event(10, 10).expect_err("equal");
        assert_eq!(equal.start(), SubtitleTime::from_micros(10));
        assert_eq!(equal.end(), SubtitleTime::from_micros(10));
        assert!(event(10, 5).is_err());
    }

    #[test]
    fn accepts_minimal_one_microsecond_interval() {
        let accepted = event(10, 11).expect("valid");
        assert_eq!(accepted.start().as_micros(), 10);
        assert_eq!(accepted.end().as_micros(), 11);
        assert_eq!(accepted.layer(), SubtitleLayer::DEFAULT);
    }

    #[test]
    fn activity_is_half_open() {
        let item = event(100, 200).expect("valid");
        assert!(!item.is_active_at(SubtitleTime::from_micros(99)));
        assert!(item.is_active_at(SubtitleTime::from_micros(100)));
        assert!(item.is_active_at(SubtitleTime::from_micros(199)));
        assert!(!item.is_active_at(SubtitleTime::from_micros(200)));
    }

    #[test]
    fn layers_order_by_value() {
        assert!(SubtitleLayer::new(-1) < SubtitleLayer::DEFAULT);
        assert_eq!(SubtitleLayer::new(5).get(), 5);
    }

    fn karaoke_event(start: i64, end: i64, reveal: i64) -> SubtitleEvent {
        let mut hidden_word = TextSpan::text("word", SpanStyle::default());
        hidden_word.reveal_at = Some(SubtitleTime::from_micros(reveal));
        let content = SubtitleContent::Text(SubtitleTextCue {
            text: StyledText::new(vec![
                TextSpan::text("now ", SpanStyle::default()),
                hidden_word,
            ]),
            placement: CuePlacement::default(),
        });
        SubtitleEvent::new(
            SubtitleTime::from_micros(start),
            SubtitleTime::from_micros(end),
            SubtitleLayer::new(3),
            content,
        )
        .expect("valid")
    }

    fn reveal_times(event: &SubtitleEvent) -> Vec<Option<i64>> {
        match event.content() {
            SubtitleContent::Text(cue) => cue
                .text
                .spans()
                .iter()
                .map(|span| span.reveal_at.map(SubtitleTime::as_micros))
                .collect(),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn shift_moves_start_end_and_reveal_but_keeps_duration_layer_and_text() {
        let original = karaoke_event(1_000, 5_000, 2_500);
        let shifted = original
            .shifted_by(SubtitleTimeShift::from_micros(10_000))
            .expect("no overflow");
        assert_eq!(shifted.start().as_micros(), 11_000);
        assert_eq!(shifted.end().as_micros(), 15_000);
        assert_eq!(reveal_times(&shifted), [None, Some(12_500)]);
        assert_eq!(shifted.layer(), SubtitleLayer::new(3));
        assert_eq!(
            shifted.end().as_micros() - shifted.start().as_micros(),
            original.end().as_micros() - original.start().as_micros()
        );
        let plain = |event: &SubtitleEvent| match event.content() {
            SubtitleContent::Text(cue) => cue.text.plain_text(),
            _ => panic!("expected text"),
        };
        assert_eq!(plain(&shifted), plain(&original));
        // Исходное событие не менялось.
        assert_eq!(original.start().as_micros(), 1_000);
    }

    #[test]
    fn negative_shift_moves_earlier_and_zero_shift_is_identity() {
        let original = karaoke_event(10_000, 20_000, 12_000);
        let earlier = original
            .shifted_by(SubtitleTimeShift::from_micros(-4_000))
            .expect("fits");
        assert_eq!(earlier.start().as_micros(), 6_000);
        assert_eq!(reveal_times(&earlier), [None, Some(8_000)]);
        assert_eq!(original.shifted_by(SubtitleTimeShift::ZERO), Ok(original));
    }

    #[test]
    fn shift_overflow_in_end_or_reveal_is_an_error_not_saturation() {
        let near_max_end = event(0, i64::MAX - 5).expect("valid");
        let error = near_max_end
            .shifted_by(SubtitleTimeShift::from_micros(10))
            .expect_err("end overflows");
        assert_eq!(error.time().as_micros(), i64::MAX - 5);
        assert_eq!(error.shift().as_micros(), 10);
        let late_reveal = karaoke_event(0, 100, i64::MAX);
        assert!(
            late_reveal
                .shifted_by(SubtitleTimeShift::from_micros(1))
                .is_err()
        );
        let early = event(i64::MIN + 1, 0).expect("valid");
        assert!(
            early
                .shifted_by(SubtitleTimeShift::from_micros(-2))
                .is_err()
        );
    }

    #[test]
    fn timeline_built_from_shifted_event_finds_cue_at_shifted_time() {
        let shifted = karaoke_event(1_000_000, 2_000_000, 1_500_000)
            .shifted_by(SubtitleTimeShift::from_millis(60_000))
            .expect("fits");
        let (timeline, _) = crate::SubtitleTimeline::from_events([shifted]);
        let mut active = Vec::new();
        timeline.collect_active_at(SubtitleTime::from_millis(1_500), &mut active);
        assert!(active.is_empty(), "old time must be empty");
        timeline.collect_active_at(SubtitleTime::from_millis(61_500), &mut active);
        assert_eq!(active.len(), 1);
    }
}
