//! Словарь API слоя оверлеев: настройки, запрос рисования, исходы, ошибки и счётчики.

use gles2_egui::{GlAccessError, ShaderError};
use thiserror::Error;

use crate::geometry::{PixelRect, RectOutOfRange};
use crate::gl_calls::VideoGlError;
use crate::layer_api::PlaneFormatPreference;
use crate::planes::PlaneSourceError;

/// Как грузить строки картинки, у которой шаг строк больше видимой ширины.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayRowUploadPreference {
    /// Один вызов с `GL_UNPACK_ROW_LENGTH`, если есть `GL_EXT_unpack_subimage`, иначе по строке
    /// на вызов. Картинка без хвостов строк всегда грузится одним вызовом.
    #[default]
    BestAvailable,
    /// Всегда по строке на вызов (путь драйверов без расширения; стенды тестов).
    RowByRow,
}

/// Верхняя граница стороны атласа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AtlasSideLimit {
    /// Сколько позволяет драйвер (`GL_MAX_TEXTURE_SIZE`; без `highp` во фрагментном шейдере —
    /// не больше [`AtlasSideLimit::MEDIUM_PRECISION_MAX_SIDE`]).
    #[default]
    DriverMaximum,
    /// Не больше заданного (стенды тестов: «картинка больше лимита пропускается»). Предел драйвера
    /// и точности всё равно действует.
    AtMost(u32),
}

impl AtlasSideLimit {
    /// Сторона атласа, которую точно адресует `mediump` (fp16, относительная точность 2⁻¹⁰ по
    /// GLSL ES 1.00 §4.5.2): нормированная координата у края атласа ошибается не больше чем на
    /// 2⁻¹⁰, а выборка `NEAREST` берёт нужный тексель, пока ошибка меньше половины текселя
    /// `1 / (2 · side)`. При `side = 256` половина текселя — 2⁻⁹, запас вдвое; при 512 запаса нет.
    pub const MEDIUM_PRECISION_MAX_SIDE: u32 = 256;
}

/// Настройки слоя (вызывающий задаёт их явно, crate ничего не хардкодит).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverlayLayerOptions {
    /// Формат атласа масок: `GL_RED_EXT` или `GL_LUMINANCE`.
    pub coverage_format: PlaneFormatPreference,
    /// Загрузка строк с хвостами.
    pub row_upload: OverlayRowUploadPreference,
    /// Предел стороны атласа.
    pub atlas_side_limit: AtlasSideLimit,
}

/// Левые верхние углы холста на цели для обеих групп, в пикселях цели (сверху вниз). Значения
/// должны быть целыми (округляет вызывающий): дробный угол сдвинул бы пиксели холста между
/// пикселями цели.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayCanvasOrigins {
    /// Угол холста основной группы `[x, y]`.
    pub main: [f32; 2],
    /// Угол холста фиксированной группы `[x, y]`.
    pub fixed: [f32; 2],
}

/// Запрос рисования подготовленного оверлея.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayDrawRequest {
    /// Где лежит холст каждой группы.
    pub origins: OverlayCanvasOrigins,
    /// Куда можно рисовать (видимая область видео), пиксели цели сверху вниз; слой пересекает её с
    /// целью сам.
    pub clip: PixelRect,
}

/// Что сделал `prepare` с атласом одного вида пикселей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAtlasOutcome {
    /// Картинок этого вида нет.
    NotNeeded,
    /// Пиксели те же, что уже в атласе: загрузки нет (вершины обновлены).
    AlreadyResident {
        /// Картинок в атласе.
        bitmaps: usize,
    },
    /// Атлас загружен заново.
    Uploaded {
        /// Картинок загружено.
        bitmaps: usize,
    },
    /// Картинки не влезли в атлас даже предельного размера: этот вид в кадре не рисуется.
    DoesNotFit {
        /// Сколько картинок пропущено.
        bitmaps: usize,
    },
}

/// Итог `prepare`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayPrepareReport {
    /// Атлас масок.
    pub coverage: OverlayAtlasOutcome,
    /// Атлас RGBA-картинок.
    pub picture: OverlayAtlasOutcome,
    /// Картинок, отброшенных до упаковки: сторона больше предела атласа.
    pub larger_than_atlas_limit: usize,
    /// Сколько картинок будет нарисовано.
    pub drawable_bitmaps: usize,
}

/// Итог рисования.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayDrawOutcome {
    /// Нарисовано.
    Drawn {
        /// Картинок.
        bitmaps: usize,
        /// Draw-вызовов (по одному на серию).
        draw_calls: usize,
    },
    /// Нечего рисовать: ничего не подготовлено или всё подготовленное пропущено.
    NothingPrepared,
    /// Область рисования пуста (видео не видно или вне цели): цель не тронута.
    NothingVisible,
}

/// Отказ создания или удаления слоя.
#[derive(Debug, Error)]
pub enum OverlayLayerError {
    /// Контекст недоступен.
    #[error("GLES2 context unavailable: {0}")]
    GlAccess(#[source] GlAccessError),
    /// Программа не собралась.
    #[error("overlay layer shader: {0}")]
    Shader(#[source] ShaderError),
    /// В слинкованной программе нет нужного входа.
    #[error("overlay layer shader has no input `{name}`")]
    MissingShaderInput {
        /// Имя атрибута или uniform.
        name: &'static str,
    },
    /// Вызов с контекстом, которому слой не принадлежит.
    #[error("overlay layer used with a foreign GLES2 context")]
    ForeignContext,
}

/// Отказ подготовки (загрузки) оверлея. После отказа слой ничего не рисует до следующего
/// успешного `prepare`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OverlayPrepareError {
    /// Кадр чужого контекста.
    #[error("overlay layer used with a foreign GLES2 context")]
    ForeignContext,
    /// Пиксели картинки не сходятся с её размером (нарушение контракта вызывающего; проверка до
    /// GL, атлас не тронут).
    #[error("overlay bitmap {index} pixels do not match its size: {source}")]
    MalformedBitmap {
        /// Номер картинки в списке.
        index: usize,
        /// Что не сошлось.
        #[source]
        source: PlaneSourceError,
    },
    /// Отказ GL (выделение атласа, буфера, загрузка).
    #[error("overlay layer GL: {0}")]
    Gl(#[source] VideoGlError),
}

/// Отказ рисования.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OverlayDrawError {
    /// Кадр чужого контекста.
    #[error("overlay layer used with a foreign GLES2 context")]
    ForeignContext,
    /// Область рисования не помещается в координаты GL.
    #[error(transparent)]
    RectOutOfRange(#[from] RectOutOfRange),
    /// Отказ GL (раскладка вершин, диапазон draw-вызова).
    #[error("overlay layer GL: {0}")]
    Gl(#[source] VideoGlError),
}

/// Счётчики атласа одного вида пикселей (имена — как у счётчиков wgpu-прохода субтитров).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayAtlasStats {
    /// Загрузок атласа (смен набора пикселей).
    pub atlas_uploads: u64,
    /// Картинок загружено.
    pub bitmap_uploads: u64,
    /// Выделений текстуры атласа (первое и каждый рост).
    pub atlas_recreations: u64,
    /// Подготовок, в которых часть картинок пропущена (не влезли).
    pub skipped_prepares: u64,
    /// Картинок пропущено.
    pub skipped_bitmaps: u64,
    /// Вызовов `glTexSubImage2D`.
    pub upload_calls: u64,
    /// Сторона текстуры атласа сейчас; 0 — текстуры нет (у картинок — пока их не было ни разу).
    pub atlas_side: u32,
}

/// Счётчики слоя.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayLayerStats {
    /// Атлас масок.
    pub coverage: OverlayAtlasStats,
    /// Атлас RGBA-картинок.
    pub picture: OverlayAtlasStats,
    /// Смен значений uniform-ов (размер цели, углы холста групп) между рисованиями.
    pub uniform_updates: u64,
    /// Draw-вызовов.
    pub draw_calls: u64,
}
