//! Перевод региона TTML (`origin`/`extent`, `textAlign`, `displayAlign`) в `CuePlacement`.
//!
//! Растеризатор (`subtitle-raster/src/appearance/placement.rs`) понимает поля WebVTT, поэтому
//! прямоугольник региона выражается ими так:
//!
//! - Горизонталь. `size` = ширина региона. Текст прижат к левому краю региона (`start`/`left`):
//!   `position` = левый край, `position_align` = `LineLeft`; по центру — `position` = середина,
//!   `Center`; к правому (`end`/`right`) — `position` = правый край, `LineRight`.
//! - Вертикаль. `displayAlign=before` — верх блока на верхней границе региона:
//!   `line` = верх, `line_align` = `Start`. `after` — низ блока на нижней границе:
//!   `line` = верх + высота, `line_align` = `End`. `center` — `line` = середина,
//!   `line_align` = `Center`; растеризатор пока приравнивает `Center` к `Start`, поэтому
//!   такой текст начинается на середине региона (известное ограничение, см. матрицу).
//! - Без региона размещение по умолчанию: внизу по центру (`line: Auto`).
//!
//! Ограничения: растеризатор умножает `size` на `max_line_width` стиля, поэтому блок может
//! быть уже региона (но всегда остаётся внутри него); отступы (`padding`) региона не учитываются.
//! Если `textAlign` не задан, текст центрируется (TTML требует `start`; для субтитров
//! центровка — привычное значение плееров).

use subtitle_core::{
    CueLine, CueLineAlign, CuePlacement, CuePosition, CuePositionAlign, CueTextAlign,
    PercentOfVideo,
};

use super::layout::RegionRect;
use super::style::{DisplayAlign, StyleSet};

/// Размещение абзаца. `rect == None` — абзац не привязан к известному региону.
///
/// `style` — итоговые свойства абзаца (с учётом региона): из них берутся
/// `textAlign`, `displayAlign` и направление письма.
pub(super) fn cue_placement(rect: Option<RegionRect>, style: StyleSet) -> CuePlacement {
    let text_align = style.text_align.unwrap_or(CueTextAlign::Center);
    let mut placement = CuePlacement {
        text_align,
        writing_direction: style.writing_direction.unwrap_or_default(),
        ..CuePlacement::default()
    };
    let Some(rect) = rect else {
        return placement;
    };
    let (position_x, position_align) = match text_align {
        CueTextAlign::Start | CueTextAlign::Left => (rect.left, CuePositionAlign::LineLeft),
        CueTextAlign::Center => (rect.left + rect.width / 2.0, CuePositionAlign::Center),
        CueTextAlign::End | CueTextAlign::Right => {
            (rect.left + rect.width, CuePositionAlign::LineRight)
        }
    };
    placement.position = CuePosition::Percent(percent(position_x));
    placement.position_align = position_align;
    placement.size = percent(rect.width);

    let (line_y, line_align) = match style.display_align.unwrap_or(DisplayAlign::Before) {
        DisplayAlign::Before => (rect.top, CueLineAlign::Start),
        DisplayAlign::Center => (rect.top + rect.height / 2.0, CueLineAlign::Center),
        DisplayAlign::After => (rect.top + rect.height, CueLineAlign::End),
    };
    placement.line = CueLine::Percent(percent(line_y));
    placement.line_align = line_align;
    placement
}

/// Процент в допустимом диапазоне (значения уже ограничены раскладкой; запасной путь — 0).
fn percent(value: f32) -> PercentOfVideo {
    PercentOfVideo::new(value.clamp(0.0, 100.0)).unwrap_or(PercentOfVideo::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: RegionRect = RegionRect {
        left: 10.0,
        top: 70.0,
        width: 80.0,
        height: 20.0,
    };

    fn style(text_align: CueTextAlign, display_align: DisplayAlign) -> StyleSet {
        StyleSet {
            text_align: Some(text_align),
            display_align: Some(display_align),
            ..StyleSet::default()
        }
    }

    #[test]
    fn no_region_keeps_default_bottom_placement() {
        let placement = cue_placement(None, StyleSet::default());
        assert_eq!(placement, CuePlacement::default());
    }

    #[test]
    fn bottom_aligned_centered_region_pins_the_text_bottom_to_the_region_bottom() {
        let placement = cue_placement(Some(RECT), style(CueTextAlign::Center, DisplayAlign::After));
        assert_eq!(placement.line, CueLine::Percent(percent(90.0)));
        assert_eq!(placement.line_align, CueLineAlign::End);
        assert_eq!(placement.position, CuePosition::Percent(percent(50.0)));
        assert_eq!(placement.position_align, CuePositionAlign::Center);
        assert_eq!(placement.size, percent(80.0));
    }

    #[test]
    fn before_aligned_region_pins_the_text_top_to_the_region_top() {
        let placement = cue_placement(Some(RECT), style(CueTextAlign::Left, DisplayAlign::Before));
        assert_eq!(placement.line, CueLine::Percent(percent(70.0)));
        assert_eq!(placement.line_align, CueLineAlign::Start);
        assert_eq!(placement.position, CuePosition::Percent(percent(10.0)));
        assert_eq!(placement.position_align, CuePositionAlign::LineLeft);
    }

    #[test]
    fn end_alignment_anchors_to_the_right_edge() {
        let placement = cue_placement(Some(RECT), style(CueTextAlign::End, DisplayAlign::Before));
        assert_eq!(placement.position, CuePosition::Percent(percent(90.0)));
        assert_eq!(placement.position_align, CuePositionAlign::LineRight);
    }

    #[test]
    fn center_display_align_uses_the_region_middle() {
        let placement = cue_placement(
            Some(RECT),
            style(CueTextAlign::Center, DisplayAlign::Center),
        );
        assert_eq!(placement.line, CueLine::Percent(percent(80.0)));
        assert_eq!(placement.line_align, CueLineAlign::Center);
    }
}
