//! Перевод `SubtitleAppearance` + `CuePlacement` в `EventStyle` конвейера.
//!
//! Это единственное место, где относительные размеры стиля превращаются в пиксели
//! конкретного холста. Для плашки результат содержит ещё и [`PlateStyle`] — её рисует
//! не текстовый конвейер, а отдельный модуль `plate`.

use subtitle_core::{CuePlacement, SubtitleColor};

use super::placement::{PlateInset, place_block};
use super::{SubtitleAppearance, TextDecoration};
use crate::event_fade::EventFade;
use crate::event_style::{
    AxisPair, BaseDirection, BorderStyle, EventPlacement, EventStyle, StyleColors, WrapStyle,
};
use crate::fonts::{FontStyle, FontWeightClass};
use crate::overlay::CanvasSize;

/// В какую сторону сдвигать реплику, если она налезает на другую
/// (libass: `shift_direction` — нижние поднимаются, остальные опускаются).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CollisionDirection {
    /// Вверх (реплика прижата к низу).
    Up,
    /// Вниз (реплика прижата к верху или центру).
    Down,
}

/// Параметры плашки в пикселях холста.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlateStyle {
    /// Цвет плашки.
    pub(crate) color: SubtitleColor,
    /// Отступ от плотной рамки букв строки по горизонтали.
    pub(crate) horizontal_padding: f64,
    /// Отступ от верха/низа ячейки строки.
    pub(crate) vertical_padding: f64,
    /// Радиус скругления.
    pub(crate) corner_radius: f64,
}

/// Всё, что нужно для рисования одной реплики.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CueStyle {
    /// Стиль текстового конвейера.
    pub(crate) event: EventStyle,
    /// Плашка за строками; `None` для обводки и тени.
    pub(crate) plate: Option<PlateStyle>,
    /// Направление сдвига при столкновении реплик.
    pub(crate) collision: CollisionDirection,
}

/// Строит стиль реплики для холста `canvas`.
pub(crate) fn cue_style(
    appearance: &SubtitleAppearance,
    placement: &CuePlacement,
    canvas: CanvasSize,
) -> CueStyle {
    // «em» стиля — высота ячейки шрифта в пикселях (см. документацию модуля appearance).
    let cell_height = f64::from(appearance.font_size.get()) * f64::from(canvas.height());
    let em = |value: super::EmSize| f64::from(value.get()) * cell_height;

    let (border, shadow, colors, plate) = match appearance.decoration {
        TextDecoration::BackgroundBox(decoration) => (
            AxisPair::uniform(0.0),
            AxisPair::uniform(0.0),
            StyleColors {
                primary: appearance.text_color,
                secondary: appearance.text_color,
                outline: SubtitleColor::TRANSPARENT,
                back: SubtitleColor::TRANSPARENT,
            },
            Some(PlateStyle {
                color: decoration.color,
                horizontal_padding: em(decoration.horizontal_padding_em),
                vertical_padding: em(decoration.vertical_padding_em),
                corner_radius: em(decoration.corner_radius_em),
            }),
        ),
        TextDecoration::OutlineAndShadow(decoration) => (
            AxisPair::uniform(em(decoration.outline_width_em)),
            AxisPair::uniform(em(decoration.shadow_offset_em)),
            StyleColors {
                primary: appearance.text_color,
                secondary: appearance.text_color,
                outline: decoration.outline_color,
                back: decoration.shadow_color,
            },
            None,
        ),
    };

    let inset = plate.map_or(
        PlateInset {
            horizontal: 0.0,
            vertical: 0.0,
        },
        |plate| PlateInset {
            horizontal: plate.horizontal_padding,
            vertical: plate.vertical_padding,
        },
    );
    let block = place_block(appearance, placement, canvas, cell_height, inset);
    let collision = match block.alignment {
        subtitle_core::ScreenAnchor::BottomLeft
        | subtitle_core::ScreenAnchor::BottomCenter
        | subtitle_core::ScreenAnchor::BottomRight => CollisionDirection::Up,
        _ => CollisionDirection::Down,
    };

    CueStyle {
        event: EventStyle {
            font_family: None,
            font_size: cell_height,
            font_weight: FontWeightClass::Regular,
            font_slant: FontStyle::Normal,
            border_style: BorderStyle::OutlineAndShadow,
            border,
            shadow,
            colors,
            alignment: block.alignment,
            margins: block.margins,
            placement: EventPlacement::ByMargins,
            wrap_style: WrapStyle::Smart,
            line_spacing: 0.0,
            // Арабский и иврит в простых субтитрах должны идти справа налево сами.
            base_direction: BaseDirection::Auto,
            fade: EventFade::NONE,
        },
        plate,
        collision,
    }
}
