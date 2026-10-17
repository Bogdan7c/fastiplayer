//! Событие субтитров: интервал времени + содержимое.

use crate::ass::AssCue;
use crate::placement::CuePlacement;
use crate::styled_text::StyledText;
use crate::time::SubtitleTime;

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
/// делает растеризатор). Bitmap-вариант (PGS, сессия 11) будет добавлен вместе с потребителем;
/// заглушек заранее нет, поэтому enum `non_exhaustive`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SubtitleContent {
    Text(SubtitleTextCue),
    Ass(AssCue),
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

    /// Полуоткрытый интервал: момент `start` входит, момент `end` уже нет.
    pub fn is_active_at(&self, time: SubtitleTime) -> bool {
        self.start <= time && time < self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
