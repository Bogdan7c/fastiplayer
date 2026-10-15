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
    /// Поля.
    pub(crate) margins: TextMargins,
    /// Режим переноса.
    pub(crate) wrap_style: WrapStyle,
    /// Дополнительный интервал между строками, пиксели.
    pub(crate) line_spacing: f64,
    /// Базовое направление абзаца.
    pub(crate) base_direction: BaseDirection,
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
