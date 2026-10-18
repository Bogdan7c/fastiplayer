//! Разрешённый стиль реплики в терминах `Style` из libass, все размеры — в пикселях кадра.
//!
//! [`EventStyle`] — мост между источниками стиля и текстовым конвейером:
//! - волна 3b строит его из пользовательского `SubtitleAppearance` (настройки плеера);
//! - сессия 08 строит его из стиля ASS-дорожки.
//!
//! Конвейер ничего не знает о том, откуда взялся стиль: он видит только готовые числа.
//! Масштабирование «PlayRes -> кадр» делает тот, кто строит стиль (в libass это
//! `screen_scale_y` и `border_scale_*`); здесь уже пиксели холста.
//!
//! Размер шрифта — «высота ячейки» в смысле libass: сумма ascent+descent лица равна размеру
//! (`FT_SIZE_REQUEST_TYPE_REAL_DIM` по Windows-метрикам лица, см. `glyph_render`).

use subtitle_core::{ScreenAnchor, SubtitleColor};

use crate::event_fade::EventFade;
use crate::fonts::{FontStyle, FontWeightClass};

/// Вид подложки и обводки (libass: `Style.BorderStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BorderStyle {
    /// Обводка и тень вокруг букв (значение 1).
    OutlineAndShadow,
    /// Непрозрачная плашка на каждый глиф цветом обводки (значение 3, как у VSFilter).
    OpaqueBox,
    /// Одна плашка цветом тени под всей репликой (значение 4, расширение libass).
    EventBackground,
}

/// Режим переноса строк (libass: `WrapStyle`). Значение 3 («нижняя строка шире») в libass
/// реализовано как 0, поэтому отдельного варианта нет: сессия 08 отобразит 3 в `Smart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WrapStyle {
    /// Умный перенос с выравниванием длин строк (значение 0).
    Smart,
    /// Перенос в конце строки без выравнивания длин (значение 1).
    EndOfLine,
    /// Только явные переводы строк (значение 2).
    NoWrap,
}

/// Где автоматический перенос строк разрешён кроме явных `\n`.
///
/// libass без `libunibreak` переносит только после пробела, поэтому текст на китайском или
/// японском без пробелов у ASS остаётся одной строкой. Для простых форматов (SRT, WebVTT,
/// srv3, TTML, mov_text, web) владелец решил переносить такой текст между иероглифами;
/// правила — в `layout::break_rules`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BreakOpportunities {
    /// Только после пробела: побитовое поведение libass (все события ASS).
    SpacesOnly,
    /// Пробелы плюс границы с CJK-символами (упрощённый UAX #14: класс ID) с правилами кинсоку.
    SpacesAndCjk,
}

/// Базовое направление абзаца для двунаправленного текста.
///
/// libass выбирает его по кодировке стиля: `Encoding = -1` — автоопределение, любое другое
/// значение — слева направо (`ass_resolve_base_direction`). Обычный стиль SRT/ASS имеет
/// `Encoding = 1`, то есть слева направо; для арабского и иврита автоопределение удобнее,
/// поэтому решение выносится в стиль и принимает его строящий код.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaseDirection {
    /// Абзац слева направо независимо от текста (поведение libass при `Encoding != -1`).
    LeftToRight,
    /// Направление абзаца по первой сильной букве (поведение libass при `Encoding = -1`).
    Auto,
}

/// Пара значений по осям в пикселях кадра.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AxisPair {
    /// По горизонтали.
    pub(crate) horizontal: f64,
    /// По вертикали.
    pub(crate) vertical: f64,
}

impl AxisPair {
    /// Одно и то же значение по обеим осям (как `Outline`/`Shadow` в стиле ASS).
    pub(crate) fn uniform(value: f64) -> Self {
        Self {
            horizontal: value,
            vertical: value,
        }
    }

    /// Хотя бы одна ось ненулевая.
    pub(crate) fn is_nonzero(&self) -> bool {
        self.horizontal != 0.0 || self.vertical != 0.0
    }
}

/// Четыре цвета стиля ASS (прямая, не инвертированная альфа: 255 = непрозрачный).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StyleColors {
    /// Заливка букв (`PrimaryColour`).
    pub(crate) primary: SubtitleColor,
    /// Вторичный цвет (`SecondaryColour`): нужен только для решения «закрашивать ли обводку под
    /// заливкой» и для караоке.
    pub(crate) secondary: SubtitleColor,
    /// Обводка (`OutlineColour`).
    pub(crate) outline: SubtitleColor,
    /// Тень и подложка события (`BackColour`).
    pub(crate) back: SubtitleColor,
}

/// Поля вокруг текста в пикселях кадра (libass: `MarginL/R/V`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextMargins {
    /// Слева.
    pub(crate) left: i32,
    /// Справа.
    pub(crate) right: i32,
    /// Сверху для верхнего якоря и снизу для нижнего.
    pub(crate) vertical: i32,
}

/// Привязка блока текста к холсту для реплик ASS с `\pos` (libass: `EVENT_POSITIONED`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum BlockAnchor {
    /// Блок прижат к краю по выравниванию реплики (`valign`) и полю `vertical_margin`.
    ByAlignment,
    /// Опорная точка блока (угол или середина по выравниванию, `get_base_point`) стоит в
    /// этой точке холста, пиксели.
    AtPoint {
        /// Координата по горизонтали.
        x: f64,
        /// Координата по вертикали.
        y: f64,
    },
}

/// Положение блока, которое задаёт прокрутка (`Banner`, `Scroll up`, `Scroll down`; libass:
/// `EVENT_HSCROLL` / `EVENT_VSCROLL`). Прокрутка перекрывает ОДНУ ось привязки
/// [`BlockAnchor`], вторая ось остаётся такой, какой её дал `\pos` или выравнивание.
/// Координаты — пиксели холста.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ScrollPlacement {
    /// Прокрутки нет.
    None,
    /// `Banner` справа налево (`SCROLL_RL`): начало блока (`device_x`) стоит в `origin_x`.
    RightToLeft {
        /// Горизонтальная координата начала блока.
        origin_x: f64,
    },
    /// `Banner` слева направо (`SCROLL_LR`): правый край блока стоит в `right_edge_x`.
    LeftToRight {
        /// Горизонтальная координата правого края блока.
        right_edge_x: f64,
    },
    /// `Scroll down` (`SCROLL_TB`): нижний край блока стоит в `bottom_edge_y`.
    TopToBottom {
        /// Вертикальная координата нижнего края блока.
        bottom_edge_y: f64,
    },
    /// `Scroll up` (`SCROLL_BT`): верхний край блока стоит в `top_edge_y`.
    BottomToTop {
        /// Вертикальная координата верхнего края блока.
        top_edge_y: f64,
    },
}

impl ScrollPlacement {
    /// Горизонтальная прокрутка (`EVENT_HSCROLL`): строки выравниваются по самой широкой
    /// строке блока, а не по области текста (`align_lines`).
    pub(crate) fn is_horizontal(self) -> bool {
        matches!(self, Self::RightToLeft { .. } | Self::LeftToRight { .. })
    }
}

/// Точная геометрия блока текста реплики ASS в пикселях холста (дробные числа libass:
/// `x2scr_left`, `max_text_width`, `y2scr_*`). Поля SRT/WebVTT целые и живут в
/// [`EventStyle::margins`]; ASS-масштаб `PlayRes -> кадр` целым не бывает.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ExactPlacement {
    /// Левый край области текста.
    pub(crate) text_left: f64,
    /// Ширина области текста (libass: `max_text_width`): по ней переносятся и выравниваются строки.
    pub(crate) text_width: f64,
    /// Вертикальное поле для верхнего и нижнего якоря, пиксели.
    pub(crate) vertical_margin: f64,
    /// Привязка блока.
    pub(crate) anchor: BlockAnchor,
    /// Прокрутка (`Effect`); перекрывает одну ось привязки.
    pub(crate) scroll: ScrollPlacement,
    /// Центр поворота.
    pub(crate) rotation_origin: RotationOrigin,
    /// `par_scale_x` libass: во сколько раз растянуть текст по горизонтали, когда пропорции
    /// хранения видео отличаются от пропорций кадра (1.0 — не отличаются).
    pub(crate) pixel_aspect_x: f64,
}

/// Центр поворота реплики ASS (`\frx \fry \frz \fax`-геометрия вращается вокруг него).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RotationOrigin {
    /// Опорная точка блока по выравниванию реплики (libass: `get_base_point`), когда `\org`
    /// не задан.
    ByAlignment,
    /// Точка `\org` в пикселях холста (libass: `x2scr_pos(org_x)`, `y2scr_pos(org_y)`).
    AtPoint {
        /// Координата по горизонтали.
        x: f64,
        /// Координата по вертикали.
        y: f64,
    },
}

/// Откуда берётся положение блока текста.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum EventPlacement {
    /// Целые поля [`EventStyle::margins`]: блок занимает полосу между ними (SRT, WebVTT).
    ByMargins,
    /// Точная геометрия ASS.
    Exact(ExactPlacement),
}

/// Стиль одной реплики.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EventStyle {
    /// Имя семейства шрифта; `None` — встроенный Noto Sans.
    pub(crate) font_family: Option<String>,
    /// Размер шрифта («высота ячейки») в пикселях кадра.
    pub(crate) font_size: f64,
    /// Насыщенность по умолчанию (спан может запросить Bold поверх).
    pub(crate) font_weight: FontWeightClass,
    /// Наклон по умолчанию.
    pub(crate) font_slant: FontStyle,
    /// Вид подложки и обводки.
    pub(crate) border_style: BorderStyle,
    /// Толщина обводки по осям, пиксели.
    pub(crate) border: AxisPair,
    /// Смещение тени по осям, пиксели (отрицательное — влево/вверх).
    pub(crate) shadow: AxisPair,
    /// Цвета.
    pub(crate) colors: StyleColors,
    /// Выравнивание на экране.
    pub(crate) alignment: ScreenAnchor,
    /// Поля (используются при [`EventPlacement::ByMargins`]).
    pub(crate) margins: TextMargins,
    /// Откуда брать положение блока: целые поля или точная геометрия ASS.
    pub(crate) placement: EventPlacement,
    /// Режим переноса.
    pub(crate) wrap_style: WrapStyle,
    /// Где разрешены автоматические переносы (пробелы или ещё и границы CJK-символов).
    pub(crate) break_opportunities: BreakOpportunities,
    /// Дополнительный интервал между строками, пиксели.
    pub(crate) line_spacing: f64,
    /// Базовое направление абзаца.
    pub(crate) base_direction: BaseDirection,
    /// Затухание реплики в момент показа (`\fad`, `\fade`); у SRT/WebVTT его нет.
    pub(crate) fade: EventFade,
}

impl EventStyle {
    /// Левый край области текста в пикселях холста.
    pub(crate) fn text_left(&self) -> f64 {
        match self.placement {
            EventPlacement::ByMargins => f64::from(self.margins.left),
            EventPlacement::Exact(exact) => exact.text_left,
        }
    }

    /// Ширина области текста (libass: `max_text_width`).
    pub(crate) fn text_width(&self, canvas_width: f64) -> f64 {
        match self.placement {
            EventPlacement::ByMargins => {
                canvas_width - f64::from(self.margins.right) - f64::from(self.margins.left)
            }
            EventPlacement::Exact(exact) => exact.text_width,
        }
    }

    /// Вертикальное поле для верхнего и нижнего якоря.
    pub(crate) fn vertical_margin(&self) -> f64 {
        match self.placement {
            EventPlacement::ByMargins => f64::from(self.margins.vertical),
            EventPlacement::Exact(exact) => exact.vertical_margin,
        }
    }

    /// Растяжение по горизонтали `par_scale_x` (1.0 для SRT/WebVTT).
    pub(crate) fn pixel_aspect_x(&self) -> f64 {
        match self.placement {
            EventPlacement::ByMargins => 1.0,
            EventPlacement::Exact(exact) => exact.pixel_aspect_x,
        }
    }

    /// Центр поворота реплики; у SRT/WebVTT поворотов нет, поэтому всегда по выравниванию.
    pub(crate) fn rotation_origin(&self) -> RotationOrigin {
        match self.placement {
            EventPlacement::ByMargins => RotationOrigin::ByAlignment,
            EventPlacement::Exact(exact) => exact.rotation_origin,
        }
    }

    /// Прокрутка блока (`Effect`); у SRT/WebVTT её нет.
    pub(crate) fn scroll_placement(&self) -> ScrollPlacement {
        match self.placement {
            EventPlacement::ByMargins => ScrollPlacement::None,
            EventPlacement::Exact(exact) => exact.scroll,
        }
    }

    /// Привязка блока к точке (`\pos`), если она задана.
    pub(crate) fn block_anchor(&self) -> BlockAnchor {
        match self.placement {
            EventPlacement::ByMargins => BlockAnchor::ByAlignment,
            EventPlacement::Exact(exact) => exact.anchor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_pair_reports_whether_any_axis_is_nonzero() {
        assert!(!AxisPair::uniform(0.0).is_nonzero());
        assert!(AxisPair::uniform(2.0).is_nonzero());
        assert!(
            AxisPair {
                horizontal: 0.0,
                vertical: -1.0
            }
            .is_nonzero()
        );
    }
}
