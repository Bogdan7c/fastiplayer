//! Позиционирование реплики: настройки cue из WebVTT и якорь SRT/ASS.

/// Некорректный процент.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error("percent {value} is outside 0.0..=100.0")]
pub struct InvalidPercentOfVideo {
    value: f32,
}

impl InvalidPercentOfVideo {
    /// Отвергнутое значение.
    pub fn value(self) -> f32 {
        self.value
    }
}

/// Процент от размера видео, гарантированно в диапазоне 0.0..=100.0 (NaN не проходит).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct PercentOfVideo(f32);

impl PercentOfVideo {
    pub const ZERO: Self = Self(0.0);
    pub const FULL: Self = Self(100.0);

    /// Проверяет диапазон; `contains` на NaN возвращает false, поэтому NaN отвергается.
    pub fn new(value: f32) -> Result<Self, InvalidPercentOfVideo> {
        if (0.0..=100.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidPercentOfVideo { value })
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

/// Вертикальная позиция (`line:` в WebVTT).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CueLine {
    Auto,
    Percent(PercentOfVideo),
    LineNumber(i32),
}

/// Выравнивание относительно `line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CueLineAlign {
    #[default]
    Start,
    Center,
    End,
}

/// Горизонтальная позиция (`position:`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CuePosition {
    Auto,
    Percent(PercentOfVideo),
}

/// Выравнивание относительно `position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CuePositionAlign {
    #[default]
    Auto,
    LineLeft,
    Center,
    LineRight,
}

/// Выравнивание текста внутри блока (`align:`); по умолчанию в WebVTT — Center.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CueTextAlign {
    Start,
    #[default]
    Center,
    End,
    Left,
    Right,
}

/// Направление письма (`vertical:`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CueWritingDirection {
    #[default]
    Horizontal,
    VerticalRightToLeft,
    VerticalLeftToRight,
}

/// Якорь на экране по раскладке цифровой клавиатуры (`{\an1}`..`{\an9}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenAnchor {
    BottomLeft,
    BottomCenter,
    BottomRight,
    MiddleLeft,
    MiddleCenter,
    MiddleRight,
    TopLeft,
    TopCenter,
    TopRight,
}

impl ScreenAnchor {
    /// Цифра numpad (1..=9) в якорь; остальное — `None`.
    pub fn from_numpad(digit: u8) -> Option<Self> {
        Some(match digit {
            1 => Self::BottomLeft,
            2 => Self::BottomCenter,
            3 => Self::BottomRight,
            4 => Self::MiddleLeft,
            5 => Self::MiddleCenter,
            6 => Self::MiddleRight,
            7 => Self::TopLeft,
            8 => Self::TopCenter,
            9 => Self::TopRight,
            _ => return None,
        })
    }
}

/// Все настройки позиционирования реплики. `Default` совпадает со значениями WebVTT.
#[derive(Debug, Clone, PartialEq)]
pub struct CuePlacement {
    pub line: CueLine,
    pub line_align: CueLineAlign,
    pub position: CuePosition,
    pub position_align: CuePositionAlign,
    pub size: PercentOfVideo,
    pub text_align: CueTextAlign,
    pub writing_direction: CueWritingDirection,
    /// Идентификатор региона (`region:`), см. [`SubtitleRegion`].
    pub region_id: Option<String>,
    /// Якорь SRT/ASS; `None` — позицию определяют остальные поля.
    pub anchor: Option<ScreenAnchor>,
}

impl Default for CuePlacement {
    fn default() -> Self {
        Self {
            line: CueLine::Auto,
            line_align: CueLineAlign::default(),
            position: CuePosition::Auto,
            position_align: CuePositionAlign::default(),
            size: PercentOfVideo::FULL,
            text_align: CueTextAlign::default(),
            writing_direction: CueWritingDirection::default(),
            region_id: None,
            anchor: None,
        }
    }
}

/// Точка в долях видео (для якорей региона).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegionPoint {
    pub x: PercentOfVideo,
    pub y: PercentOfVideo,
}

/// Режим прокрутки региона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RegionScroll {
    #[default]
    None,
    Up,
}

/// Регион WebVTT: прямоугольник, в который складываются реплики.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleRegion {
    pub id: String,
    pub width: PercentOfVideo,
    pub lines: u32,
    /// Точка региона, которая привязывается к `viewport_anchor`.
    pub region_anchor: RegionPoint,
    pub viewport_anchor: RegionPoint,
    pub scroll: RegionScroll,
}

impl SubtitleRegion {
    /// Регион со значениями по умолчанию из спецификации WebVTT.
    pub fn with_webvtt_defaults(id: impl Into<String>) -> Self {
        let bottom_left = RegionPoint {
            x: PercentOfVideo::ZERO,
            y: PercentOfVideo::FULL,
        };
        Self {
            id: id.into(),
            width: PercentOfVideo::FULL,
            lines: 3,
            region_anchor: bottom_left,
            viewport_anchor: bottom_left,
            scroll: RegionScroll::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_accepts_bounds_and_rejects_outside_and_nan() {
        assert_eq!(PercentOfVideo::new(0.0).map(PercentOfVideo::get), Ok(0.0));
        assert_eq!(
            PercentOfVideo::new(100.0).map(PercentOfVideo::get),
            Ok(100.0)
        );
        assert_eq!(PercentOfVideo::new(42.5).map(PercentOfVideo::get), Ok(42.5));
        assert_eq!(
            PercentOfVideo::new(100.01).expect_err("above").value(),
            100.01
        );
        assert!(PercentOfVideo::new(-0.01).is_err());
        assert!(PercentOfVideo::new(f32::NAN).is_err());
        assert!(PercentOfVideo::new(f32::INFINITY).is_err());
    }

    #[test]
    fn numpad_maps_every_digit_to_distinct_anchor() {
        let anchors: Vec<_> = (1..=9).map(ScreenAnchor::from_numpad).collect();
        assert_eq!(anchors[0], Some(ScreenAnchor::BottomLeft));
        assert_eq!(anchors[4], Some(ScreenAnchor::MiddleCenter));
        assert_eq!(anchors[7], Some(ScreenAnchor::TopCenter));
        let distinct: std::collections::HashSet<_> = anchors.iter().flatten().collect();
        assert_eq!(distinct.len(), 9);
        assert_eq!(ScreenAnchor::from_numpad(0), None);
        assert_eq!(ScreenAnchor::from_numpad(10), None);
    }

    #[test]
    fn default_placement_matches_webvtt_defaults() {
        let placement = CuePlacement::default();
        assert_eq!(placement.line, CueLine::Auto);
        assert_eq!(placement.position, CuePosition::Auto);
        assert_eq!(placement.size, PercentOfVideo::FULL);
        assert_eq!(placement.text_align, CueTextAlign::Center);
        assert_eq!(placement.writing_direction, CueWritingDirection::Horizontal);
        assert_eq!(placement.region_id, None);
        assert_eq!(placement.anchor, None);
    }

    #[test]
    fn default_region_matches_webvtt_defaults() {
        let region = SubtitleRegion::with_webvtt_defaults("r1");
        assert_eq!(region.id, "r1");
        assert_eq!(region.width, PercentOfVideo::FULL);
        assert_eq!(region.lines, 3);
        assert_eq!(region.region_anchor.x, PercentOfVideo::ZERO);
        assert_eq!(region.viewport_anchor.y, PercentOfVideo::FULL);
        assert_eq!(region.scroll, RegionScroll::None);
    }
}
