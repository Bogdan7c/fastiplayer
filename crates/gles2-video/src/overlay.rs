//! Слой bitmap-оверлеев поверх видео (сессия 08 плана `user/gles2-renderer/`): субтитры плеера,
//! титры редактора — любые готовые картинки, которые надо положить пиксель-в-пиксель.
//!
//! Слой не знает субтитров: вызывающий описывает каждую картинку трейтом [`OverlayBitmap`]
//! (адаптер плеера — `render-gles2-shell`). Правила — строгий паритет с wgpu-проходом субтитров
//! плеера (`render-wgpu-shell/src/subtitle_overlay/`):
//! - два вида пикселей: маска покрытия (1 байт) + один цвет на картинку и готовая RGBA-картинка
//!   с премультиплицированной альфой (4 байта) + множитель прозрачности. У каждого вида свой
//!   атлас (маски — `GL_LUMINANCE`/`GL_RED_EXT`, картинки — RGBA8, создаётся лениво);
//! - 1 пиксель холста = 1 пиксель цели, без масштаба и фильтрации (`NEAREST` в атласе);
//! - смешивание «over» с премультиплицированной альфой: маска даёт `(rgb·cov·a, cov·a)`,
//!   картинка — `texel·a`, функция `ONE, ONE_MINUS_SRC_ALPHA`;
//! - порядок — порядок списка (первая картинка самая нижняя); подряд идущие одного вида — один
//!   draw-вызов (серия);
//! - картинки двух групп размещения ([`OverlayPlacementGroup`]) двигаются своими углами холста
//!   ([`OverlayCanvasOrigins`]) — это только uniform, атлас и вершины не перезагружаются;
//! - атлас загружается только при смене пикселей ([`OverlayBitmap::shows_same_pixels_as`]);
//!   картинка, которая не влезает в атлас, пропускается со счётчиком, соседи рисуются.
//!
//! Как устроен перенос с wgpu на GLES 2.0 (нет инстансинга, целочисленных атрибутов, flat-varying
//! и `textureLoad`):
//! - каждая картинка — 6 вершин (два треугольника), её атрибуты повторены на всех вершинах, поэтому
//!   интерполяция безвредна;
//! - координаты атласа считаются на CPU в углах квада и интерполируются: в центре пикселя
//!   получается центр текселя, выборка `NEAREST` берёт ровно его (никакой адресной арифметики во
//!   фрагментном шейдере — урок сессии 05 на T410);
//! - группа — число 0/1, угол холста выбирает `mix` между двумя uniform-ами (углы целые, `mix`
//!   с 0/1 точен).

mod atlas;
mod layer;
mod program;
mod renderer;
mod runs;
#[cfg(test)]
mod test_support;
mod upload_calls;
mod vertices;

mod api;

pub use api::{
    AtlasSideLimit, OverlayAtlasOutcome, OverlayAtlasStats, OverlayCanvasOrigins, OverlayDrawError,
    OverlayDrawOutcome, OverlayDrawRequest, OverlayLayerError, OverlayLayerOptions,
    OverlayLayerStats, OverlayPrepareError, OverlayPrepareReport, OverlayRowUploadPreference,
};
pub use renderer::BitmapOverlayRenderer;

/// Формат пикселей картинки оверлея.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverlayPixelFormat {
    /// Маска покрытия: 1 байт на пиксель (0 — пусто, 255 — полностью), цвет — [`OverlayTint`].
    Coverage,
    /// Готовая картинка RGBA8, цвет уже умножен на альфу; [`OverlayTint::alpha`] — множитель
    /// прозрачности всей картинки, остальные каналы цвета не используются.
    PremultipliedRgba,
}

impl OverlayPixelFormat {
    /// Байт на пиксель.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Coverage => 1,
            Self::PremultipliedRgba => 4,
        }
    }
}

/// Пиксели картинки в памяти вызывающего: `height` строк по `width × байт на пиксель` байт,
/// начала соседних строк — через `stride` байт.
///
/// Срез обязан покрывать все видимые байты: `(height − 1) × stride + width × байт на пиксель`.
/// Хвост последней строки не нужен — так растеризатор субтитров отдаёт окно в общем буфере. Слой
/// проверяет это до первого вызова GL ([`OverlayPrepareError::MalformedBitmap`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayPixels<'a> {
    /// Формат пикселей.
    pub format: OverlayPixelFormat,
    /// Байты, начиная с первого пикселя первой строки.
    pub bytes: &'a [u8],
    /// Шаг между началами строк в байтах.
    pub stride: usize,
}

/// Прямоугольник картинки на холсте, в пикселях холста (холст ложится на цель 1:1, его левый
/// верхний угол — [`OverlayCanvasOrigins`] группы картинки). Угол может быть отрицательным:
/// картинка частично за краем холста обрезается scissor-ом и краем цели.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OverlayBitmapRect {
    /// Левый край в пикселях холста.
    pub x: i32,
    /// Верхний край в пикселях холста.
    pub y: i32,
    /// Ширина в пикселях (не 0).
    pub width: u32,
    /// Высота в пикселях (не 0).
    pub height: u32,
}

/// Цвет картинки, прямой (не премультиплицированный) RGBA8.
///
/// Маска покрытия красится им целиком; у RGBA-картинки используется только `alpha` — множитель
/// прозрачности.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OverlayTint {
    /// Красный.
    pub red: u8,
    /// Зелёный.
    pub green: u8,
    /// Синий.
    pub blue: u8,
    /// Непрозрачность.
    pub alpha: u8,
}

/// Группа размещения картинки: у каждой группы свой угол холста ([`OverlayCanvasOrigins`]).
///
/// Так основной диалог поднимается над панелью управления, а надписи с заданной автором позицией
/// остаются на месте — и ни одна группа не перезагружает атлас.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverlayPlacementGroup {
    /// Основная группа.
    Main,
    /// Фиксированная группа.
    Fixed,
}

/// Картинка оверлея с точки зрения слоя.
///
/// Слой хранит клоны загруженных в атлас картинок, пока они там лежат: клон держит буфер
/// пикселей живым, поэтому [`Self::shows_same_pixels_as`] может сравнивать буферы по адресу —
/// адрес не достанется другой картинке, пока слой помнит прежнюю. Клон должен быть дешёвым
/// (счётчик ссылок, а не копия пикселей).
pub trait OverlayBitmap: Clone {
    /// Где картинка лежит на холсте.
    fn rect(&self) -> OverlayBitmapRect;

    /// Пиксели картинки.
    fn pixels(&self) -> OverlayPixels<'_>;

    /// Цвет маски или множитель прозрачности картинки.
    fn tint(&self) -> OverlayTint;

    /// Группа размещения.
    fn placement_group(&self) -> OverlayPlacementGroup;

    /// Показывает ли `other` те же пиксели, что `self` (тот же буфер, то же окно в нём, тот же
    /// формат). Положение, цвет и группа не сравниваются: их меняют вершины, а не атлас.
    ///
    /// Ложное «да» показало бы на экране старые пиксели, поэтому при сомнении — «нет» (лишняя
    /// загрузка безвредна).
    fn shows_same_pixels_as(&self, other: &Self) -> bool;
}
