//! Теги переопределений `{\...}` с точной семантикой аргументов libass.
//!
//! Теги слоя 1, геометрия слоя 2 (`\frx/y/z`, `\fax/y`, `\org`, `\fad`) и теги слоя 3
//! (`\p`, `\pbo`, `\clip`, `\iclip`, `\blur`, `\be`, `\t`, `\k*`) хранятся разобранными
//! в «сыром» виде: вычисления, зависящие от времени кадра, делает растеризатор.
//!
//! Значение `None` в большинстве вариантов означает «тег без допустимого аргумента:
//! вернуть значение из стиля реплики» — так libass обрабатывает `\b`, `\fn` и т.п.

use super::alignment::AssAlignment;
use super::animation_tags::AssTransform;
use super::color::AssRgb;
use super::filter_tags::AssClip;
use super::geometry_tags::{AssFade, AssRotationAxis, AssShearAxis};
use super::karaoke_tags::AssKaraoke;
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
    /// `\alpha`, `\1a`..`\4a`: прозрачность (0 — непрозрачно).
    ///
    /// Значение хранится сырым `int32` (результат `parse_alpha_tag`): libass усекает его до
    /// младшего байта только при применении, ПОСЛЕ смеси `\t` (`change_alpha`), и в смесь число
    /// входит как `uint32`. Поэтому `\t(\alpha&H1FF&)` и отрицательные значения ведут себя иначе,
    /// чем если бы байт отрезали при разборе.
    Alpha {
        target: AssAlphaTarget,
        value: Option<i32>,
    },
    /// `\bord`, `\xbord`, `\ybord`: значение как написано; зажатие снизу нулём делает
    /// растеризатор после смешивания `\t`.
    Border {
        axes: AssAxes,
        value: Option<f64>,
    },
    /// `\shad`, `\xshad`, `\yshad`: значение как написано. Растеризатор зажимает снизу нулём
    /// только `\shad` (как libass), а `\xshad`/`\yshad` могут быть отрицательными (тень
    /// влево/вверх).
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
    /// `\frx`, `\fry`, `\frz`, `\fr`: угол в градусах. `None` — тег без аргумента:
    /// для X и Y это 0, для Z — угол стиля (`Angle`).
    Rotation {
        axis: AssRotationAxis,
        degrees: Option<f64>,
    },
    /// `\fax`, `\fay`: коэффициент наклона. `None` — тег без аргумента (наклон 0).
    Shear {
        axis: AssShearAxis,
        factor: Option<f64>,
    },
    /// `\org(x,y)`: центр поворота в координатах сценария; ровно два аргумента (иначе тег
    /// игнорируется). Побеждает первый `\org` реплики, и он отключает поиск свободного места.
    Origin {
        x: f64,
        y: f64,
    },
    /// `\fad(вход,выход)` / `\fade(...)`: затухание в сыром виде, см. [`AssFade`]. Побеждает
    /// первый тег затухания реплики.
    Fade(AssFade),
    /// `\r` / `\rИмя`: сброс к стилю реплики или к стилю по имени (строгий поиск
    /// `AssScript::lookup_style_strict`; промах — стиль реплики).
    Reset(Option<String>),
    /// `\q0..\q3`; прочее -> `None` (режим из `[Script Info]`).
    WrapStyle(Option<AssWrapStyle>),
    /// `\fscx`: доля (аргумент / 100, без зажатия: ноль ставит растеризатор после `\t`);
    /// `None` — масштаб из стиля.
    ScaleX(Option<f64>),
    /// `\fscy`.
    ScaleY(Option<f64>),
    /// `\fsc`: libass игнорирует аргумент и возвращает оба масштаба к стилю.
    ResetScale,
    /// `\fsp`: межбуквенный интервал; `None` — из стиля.
    Spacing(Option<f64>),
    /// `\p<n>`: переключает режим рисунка. Текст при включённом режиме растеризуется как
    /// векторная фигура (команды `m l b s ...`), а не как буквы.
    Drawing(AssDrawingMode),
    /// `\pbo<v>`: смещение базовой линии рисунка (`double`, libass усекает до целого при
    /// сохранении в глиф). Тег без аргумента даёт `0.0` (`argtod` пустой строки).
    DrawingBaselineOffset(f64),
    /// `\blur<v>`: радиус гауссова размытия «как написано». `None` — тег без аргумента,
    /// libass жёстко сбрасывает радиус в `0` (не к стилю). Зажатие в
    /// `[0, ASS_BLUR_MAX_RADIUS]` делает растеризатор ПОСЛЕ интерполяции `\t`
    /// (`ass_parse.c`: `blur*(1-pwr)+val*pwr`, затем clamp).
    Blur(Option<f64>),
    /// `\be<v>`: «мягкий край» (box-размытие), значение `double` «как написано». `None` —
    /// тег без аргумента, libass сбрасывает в `0`. Округление `dtoi32(v + 0.5)` и зажатие в
    /// `[0, ASS_EDGE_BLUR_MAX]` делает растеризатор (после интерполяции `\t`).
    EdgeBlur(Option<f64>),
    /// `\clip(...)` / `\iclip(...)`: см. [`AssClip`]. Побеждающий вариант выбирает
    /// растеризатор: последний прямоугольный, первый векторный.
    Clip(AssClip),
    /// `\t(...)`: анимация вложенных тегов, см. [`AssTransform`]. Даже некорректный `\t`
    /// сохраняется (с пустым списком тегов): он влияет на поиск места реплики и на коэффициент
    /// внешнего `\t`.
    Transform(AssTransform),
    /// `\k`, `\K`, `\kf`, `\ko`: караоке-слог, см. [`AssKaraoke`].
    Karaoke(AssKaraoke),
    /// `\kt`: абсолютный сдвиг времени караоке в сотых долях секунды «как написано»
    /// (`argtod`). `None` — тег без аргумента (сдвиг 0).
    KaraokeSkip(Option<f64>),
}
