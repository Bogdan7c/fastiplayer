//! Теги переопределений `{\...}` с точной семантикой аргументов libass.
//!
//! Теги слоя 1 хранятся разобранными. Теги слоёв 2–3 (`\frx`, `\move`, `\t`, `\clip`,
//! `\k`, `\p` ...) распознаются, но только как [`AssOverride::Unsupported`]: растеризатор
//! их игнорирует, а счётчики попадают в отчёт разбора.
//!
//! Значение `None` в большинстве вариантов означает «тег без допустимого аргумента:
//! вернуть значение из стиля реплики» — так libass обрабатывает `\b`, `\fn` и т.п.

use super::alignment::AssAlignment;
use super::color::AssRgb;
use super::script_info::AssWrapStyle;
use super::style::AssBold;

/// Слот цвета: основной, вторичный (караоке), контур, тень.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssColorSlot {
    /// `\1c`, `\c`, `\1a`.
    Primary,
    /// `\2c`, `\2a`.
    Secondary,
    /// `\3c`, `\3a`.
    Outline,
    /// `\4c`, `\4a`.
    Back,
}

/// К каким слотам применяется прозрачность.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssAlphaTarget {
    /// `\alpha`: все четыре слота.
    All,
    /// `\1a`..`\4a`.
    Slot(AssColorSlot),
}

/// По каким осям действует тег (`\bord` — обе, `\xbord` — X, `\ybord` — Y).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssAxes {
    Both,
    X,
    Y,
}

/// Аргумент `\fs`.
///
/// Во всех вариантах итог `<= 0` libass заменяет размером из стиля: это решает растеризатор.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AssFontSize {
    /// `\fs20`: новый размер.
    Absolute(f64),
    /// `\fs+2` / `\fs-2`: аргумент со знаком — изменение в десятых долях текущего размера:
    /// `новый = текущий * (1 + значение / 10)`.
    Relative(f64),
    /// `\fs` без аргумента: размер стиля.
    Reset,
}

/// Режим векторного рисунка (`\p<n>`): `n > 0` — текст до `\p0` это команды рисования.
///
/// libass: `val = max(0, argtoi32)`, `drawing_scale = val`. Тег без аргумента даёт 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssDrawingMode {
    /// `\p0` (и `\p` без аргумента): обычный текст.
    Off,
    /// `\p<scale>`, `scale >= 1`: рисунок; `scale` — делитель координат `2^(scale-1)`.
    On { scale: u32 },
}

impl AssDrawingMode {
    /// Из аргумента тега: отрицательные значения libass превращает в 0.
    pub fn from_tag_value(value: i32) -> Self {
        match u32::try_from(value) {
            Ok(scale) if scale > 0 => Self::On { scale },
            _ => Self::Off,
        }
    }

    /// Включён ли режим рисунка.
    pub const fn is_on(self) -> bool {
        matches!(self, Self::On { .. })
    }
}

/// Аргументы `\move` в порядке libass (`ass_parse.c`): точки и, при шести аргументах,
/// границы времени.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AssMove {
    pub from_x: f64,
    pub from_y: f64,
    pub to_x: f64,
    pub to_y: f64,
    /// Начало движения в мс от начала реплики; `0` вместе с `end_ms == 0` — «на всю реплику».
    pub start_ms: i32,
    /// Конец движения в мс от начала реплики (после упорядочивания `start_ms <= end_ms`).
    pub end_ms: i32,
}

/// Теги слоёв 2–3: распознаются (чтобы не принять за другой тег), но не исполняются.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AssUnsupportedTag {
    Fax,
    Fay,
    Frx,
    Fry,
    /// `\frz` и `\fr`.
    Frz,
    Blur,
    Be,
    Move,
    Fade,
    Org,
    Transform,
    Clip,
    InverseClip,
    /// `\k`, `\K`, `\kf`, `\ko`, `\kt`.
    Karaoke,
    /// `\p<n>` с `n > 0` (учёт в отчёте; сам режим хранит [`AssOverride::Drawing`]).
    Drawing,
    /// `\pbo`.
    DrawingBaselineOffset,
}

impl AssUnsupportedTag {
    /// Каноническое имя тега для матрицы поддержки и отчёта.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Fax => "fax",
            Self::Fay => "fay",
            Self::Frx => "frx",
            Self::Fry => "fry",
            Self::Frz => "frz",
            Self::Blur => "blur",
            Self::Be => "be",
            Self::Move => "move",
            Self::Fade => "fade",
            Self::Org => "org",
            Self::Transform => "t",
            Self::Clip => "clip",
            Self::InverseClip => "iclip",
            Self::Karaoke => "k",
            Self::Drawing => "p",
            Self::DrawingBaselineOffset => "pbo",
        }
    }
}

/// Один тег блока `{...}`.
#[derive(Debug, Clone, PartialEq)]
pub enum AssOverride {
    /// `\b`: допустимы `0`, `1` и `>= 100`; прочее -> `None` (из стиля).
    Bold(Option<AssBold>),
    /// `\i`: допустимы `0` и `1`.
    Italic(Option<bool>),
    /// `\u`: допустимы `0` и `1`.
    Underline(Option<bool>),
    /// `\s`: допустимы `0` и `1`.
    StrikeOut(Option<bool>),
    /// `\fn`: `None` для `\fn` без аргумента и для `\fn0`.
    FontName(Option<String>),
    FontSize(AssFontSize),
    /// `\fe`: кодировка шрифта; `None` — из стиля.
    FontEncoding(Option<i32>),
    /// `\c`, `\1c`..`\4c`. Разбор числа как в libass: мусор даёт `0` (чёрный), а не ошибку.
    Color {
        slot: AssColorSlot,
        value: Option<AssRgb>,
    },
    /// `\alpha`, `\1a`..`\4a`: прозрачность (0 — непрозрачно), младший байт числа.
    Alpha {
        target: AssAlphaTarget,
        value: Option<u8>,
    },
    /// `\bord`, `\xbord`, `\ybord`: значение уже не меньше 0.
    Border {
        axes: AssAxes,
        value: Option<f64>,
    },
    /// `\shad`: значение не меньше 0 (как в libass), а `\xshad`/`\yshad` могут быть
    /// отрицательными (тень влево/вверх).
    Shadow {
        axes: AssAxes,
        value: Option<f64>,
    },
    /// `\an` и `\a`. Значение уже переведено в типизированное выравнивание; `None` —
    /// недопустимый аргумент (взять из стиля). Правило «побеждает ПЕРВЫЙ тег выравнивания
    /// в реплике, даже недопустимый» применяет растеризатор.
    Alignment(Option<AssAlignment>),
    /// `\pos(x,y)`: ровно два аргумента (иначе тег игнорируется).
    Position {
        x: f64,
        y: f64,
    },
    /// `\move(x1,y1,x2,y2[,t1,t2])`: как и `\pos`, фиксирует позицию реплики (повторный
    /// `\pos`/`\move` игнорируется). Анимация между точками — слой 2: растеризатор слоя 1
    /// рисует реплику в начальной точке (положение libass при `t <= start`).
    Move(AssMove),
    /// `\r` / `\rИмя`: сброс к стилю реплики или к стилю по имени (строгий поиск
    /// `AssScript::lookup_style_strict`; промах — стиль реплики).
    Reset(Option<String>),
    /// `\q0..\q3`; прочее -> `None` (режим из `[Script Info]`).
    WrapStyle(Option<AssWrapStyle>),
    /// `\fscx`: доля (аргумент / 100, не меньше 0); `None` — масштаб из стиля.
    ScaleX(Option<f64>),
    /// `\fscy`.
    ScaleY(Option<f64>),
    /// `\fsc`: libass игнорирует аргумент и возвращает оба масштаба к стилю.
    ResetScale,
    /// `\fsp`: межбуквенный интервал; `None` — из стиля.
    Spacing(Option<f64>),
    /// `\p<n>`: переключает режим рисунка. Растеризатор слоя 1 не рисует векторные
    /// рисунки: текст, встреченный при включённом режиме, пропускается и считается.
    Drawing(AssDrawingMode),
    /// Тег слоя 2–3: нужен только для счётчиков.
    Unsupported(AssUnsupportedTag),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_names_are_unique() {
        let all = [
            AssUnsupportedTag::Fax,
            AssUnsupportedTag::Fay,
            AssUnsupportedTag::Frx,
            AssUnsupportedTag::Fry,
            AssUnsupportedTag::Frz,
            AssUnsupportedTag::Blur,
            AssUnsupportedTag::Be,
            AssUnsupportedTag::Move,
            AssUnsupportedTag::Fade,
            AssUnsupportedTag::Org,
            AssUnsupportedTag::Transform,
            AssUnsupportedTag::Clip,
            AssUnsupportedTag::InverseClip,
            AssUnsupportedTag::Karaoke,
            AssUnsupportedTag::Drawing,
            AssUnsupportedTag::DrawingBaselineOffset,
        ];
        let names: std::collections::HashSet<_> = all.iter().map(|tag| tag.name()).collect();
        assert_eq!(names.len(), all.len());
    }
}
