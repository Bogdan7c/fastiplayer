//! Словарь API видеослоя: настройки, запрос рисования, исходы, ошибки и счётчики.
//!
//! Отдельно от [`crate::VideoLayerRenderer`], чтобы модуль рендера держал только логику. Отказы
//! не сливаются в `bool`: отсутствие кадра, занятость кольца (`Busy`), неподдержанный источник,
//! ошибка драйвера и чужой контекст — разные варианты.

use thiserror::Error;

use gles2_egui::{FenceWaitError, GlAccessError, ShaderError};

use crate::color::{ColorAdjustment, YuvColorEncoding};
use crate::geometry::{PictureMapping, PixelRect, QuarterTurn, RectOutOfRange};
use crate::gl_calls::VideoGlError;
use crate::hdr::HdrToSdrConversion;
use crate::planes::{FrameLayoutError, PlaneRole, PlaneSourceKind, SampleDepth};
use crate::ring::{BusyReason, UploadSlotCount};

/// Какой формат текстур плоскостей брать.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaneFormatPreference {
    /// `GL_RED_EXT`, если есть `GL_EXT_texture_rg`, иначе `GL_LUMINANCE`.
    #[default]
    BestAvailable,
    /// Только `GL_LUMINANCE` ядра (стенды тестов и драйверы с кривым `texture_rg`).
    LuminanceOnly,
}

/// Как задавать шаг строк источника.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowLengthPreference {
    /// `GL_UNPACK_ROW_LENGTH`, если есть `GL_EXT_unpack_subimage`, иначе текстура шириной в stride.
    #[default]
    BestAvailable,
    /// Всегда текстура шириной в stride (путь драйверов без расширения; стенды тестов).
    StrideWideTextures,
}

/// Настройки видеослоя (вызывающий задаёт их явно, crate ничего не хардкодит).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoLayerOptions {
    /// Слотов в кольце текстур.
    pub upload_slots: UploadSlotCount,
    /// Формат текстур плоскостей.
    pub plane_format: PlaneFormatPreference,
    /// Шаг строк при загрузке.
    pub row_length: RowLengthPreference,
}

/// Отказ создания видеослоя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VideoLayerError {
    /// Контекст недоступен (потерян, окно отпущено без surfaceless-контекста, отказ make current).
    #[error("GLES2 context unavailable: {0}")]
    GlAccess(#[source] GlAccessError),
    /// Программа не собралась.
    #[error("video layer shader: {0}")]
    Shader(#[source] ShaderError),
    /// В слинкованной программе нет нужного входа.
    #[error("video layer shader has no input `{name}`")]
    MissingShaderInput {
        /// Имя атрибута или uniform.
        name: &'static str,
    },
    /// Отказ GL при создании буфера вершин.
    #[error("video layer GL: {0}")]
    Gl(#[source] VideoGlError),
    /// Вызов с контекстом, которому видеослой не принадлежит.
    #[error("video layer used with a foreign GLES2 context")]
    ForeignContext,
}

/// Отказ загрузки кадра.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VideoUploadError {
    /// Вызов с чужим контекстом.
    #[error("video layer used with a foreign GLES2 context")]
    ForeignContext,
    /// Контекст недоступен.
    #[error("GLES2 context unavailable: {0}")]
    GlAccess(#[source] GlAccessError),
    /// Плоскость не подходит к раскладке кадра.
    #[error("frame layout: {0}")]
    Layout(#[source] FrameLayoutError),
    /// Вид источника ещё не реализован (DMA-BUF — сессии 07 и 12).
    #[error("{role:?} plane source {kind:?} is not supported by this video layer yet")]
    PlaneSourceUnsupported {
        /// Плоскость.
        role: PlaneRole,
        /// Вид источника.
        kind: PlaneSourceKind,
    },
    /// Текстура плоскости больше, чем разрешает драйвер.
    #[error("{role:?} plane needs a {width}x{height} texture, driver limit is {max}")]
    PlaneTooLarge {
        /// Плоскость.
        role: PlaneRole,
        /// Ширина текстуры (может быть stride без `EXT_unpack_subimage`).
        width: u32,
        /// Высота.
        height: u32,
        /// `GL_MAX_TEXTURE_SIZE`.
        max: u32,
    },
    /// Проверка fence не удалась (потеря контекста, ошибка драйвера).
    #[error("fence status: {0}")]
    Fence(#[source] FenceWaitError),
    /// Отказ GL при выделении или загрузке.
    #[error("video layer GL: {0}")]
    Gl(#[source] VideoGlError),
    /// Кольцо выдало слот, которого у него нет (внутренняя несогласованность; не должна
    /// случаться — индекс выдаёт само кольцо, а слоты не удаляются).
    #[error("upload ring handed out a slot it does not own")]
    RingSlotUnavailable,
    /// 10/12 бит без `highp` во фрагментном шейдере: в fp16 код до 4095 не собрать точно.
    /// Возможности рендера такие кадры не объявляют; это защита от вызывающего, который их не
    /// спросил.
    #[error("{depth:?} planes need highp fragment precision, the driver has only mediump")]
    HighBitDepthNeedsHighPrecision {
        /// Разрядность кадра.
        depth: SampleDepth,
    },
}

/// Итог загрузки кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadOutcome {
    /// Кадр скопирован в свободный слот и стал текущим.
    Uploaded(UploadReport),
    /// Этот кадр уже текущий: ничего не загружено.
    AlreadyCurrent,
    /// Свободного слота нет; текущий кадр не изменился, байты кадра не прочитаны.
    Busy(BusyReason),
}

/// Что сделала загрузка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadReport {
    /// Вызовов `glTexSubImage2D` (3 с `unpack_subimage`, до 6 без него).
    pub upload_calls: u32,
    /// Байт записано в текстуры: с `GL_UNPACK_ROW_LENGTH` — ровно видимые байты кадра, у
    /// текстур шириной в stride — ещё хвосты строк (каждый байт — один раз).
    pub copied_bytes: usize,
    /// Текстур выделено заново (0 у потока кадров одного размера).
    pub allocated_textures: u32,
}

/// Что рисовать и где.
#[derive(Debug, Clone, Copy)]
pub struct VideoDrawRequest<'a> {
    /// Область видео (по ней считается letterbox).
    pub viewport: PixelRect,
    /// Видимые части области (исключения вроде сайдбара уже вычтены): один draw на прямоугольник.
    pub visible_rects: &'a [PixelRect],
    /// Letterbox.
    pub picture: PictureMapping,
    /// Поворот картинки.
    pub orientation: QuarterTurn,
    /// Перевод цвета кадра.
    pub color: VideoColorConversion,
}

/// Как перевести цвет кадра в SDR-выход.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VideoColorConversion {
    /// SDR: матрица кодировки и цветокоррекция. 8 бит — одна матрица на кадр (сессия 05);
    /// 10/12 бит — сначала нормализация кодов по разрядности текущего кадра (с ограничением, как
    /// wgpu), затем та же матрица без множителя limited.
    Sdr {
        /// Кодировка цвета кадра.
        encoding: YuvColorEncoding,
        /// Цветокоррекция.
        adjustment: ColorAdjustment,
    },
    /// HDR → SDR BT.2446-C (только кадры 10/12 бит): нормализация → BT.2020 → таблица
    /// тон-маппинга ([`HdrToSdrConversion`]). Цветокоррекции SDR у этого пути нет — как у wgpu.
    HdrToSdr(HdrToSdrConversion),
}

/// Итог рисования.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoDrawOutcome {
    /// Цель залита чёрным, кадр нарисован.
    Drawn {
        /// Draw-вызовов (по одному на видимый прямоугольник).
        draw_calls: u32,
    },
    /// Кадра ещё нет: цель залита чёрным.
    ClearedWithoutFrame,
    /// Видимых прямоугольников нет: цель залита чёрным.
    ClearedNothingVisible,
}

/// Отказ рисования.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VideoDrawError {
    /// Кадр показа чужого контекста.
    #[error("video layer used with a foreign GLES2 context")]
    ForeignContext,
    /// Прямоугольник не помещается в координаты GL.
    #[error(transparent)]
    RectOutOfRange(#[from] RectOutOfRange),
    /// HDR → SDR запрошен для кадра 8 бит: таблица и нормализация рассчитаны на 10/12 бит (у
    /// плеера 8-битный HDR и не объявляется — wgpu рисует его как SDR, это решает адаптер).
    #[error("HDR-to-SDR needs a 10/12-bit frame, the current frame is {depth:?}")]
    HdrToSdrNeedsHighBitFrame {
        /// Разрядность текущего кадра.
        depth: SampleDepth,
    },
    /// Текущий кадр 10/12 бит, а программ 10/12 бит нет (драйвер без `highp`; такие кадры
    /// отклоняет уже загрузка, это защита).
    #[error("{depth:?} frame needs highp fragment precision, the driver has only mediump")]
    HighBitDepthNeedsHighPrecision {
        /// Разрядность кадра.
        depth: SampleDepth,
    },
    /// Атлас таблицы HDR больше лимита текстур драйвера.
    #[error("HDR tone-mapping table needs a {width}x{height} texture, driver limit is {max}")]
    HdrTableTooLarge {
        /// Ширина атласа.
        width: u32,
        /// Высота атласа.
        height: u32,
        /// `GL_MAX_TEXTURE_SIZE`.
        max: u32,
    },
    /// Драйвер не создал или не выделил текстуру таблицы HDR.
    #[error("HDR tone-mapping table texture: {0}")]
    HdrTableTexture(#[source] VideoGlError),
}

/// Счётчики видеослоя (накопительные).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoLayerStats {
    /// Кадров загружено.
    pub frames_uploaded: u64,
    /// Плоскостей загружено (3 на кадр).
    pub planes_uploaded: u64,
    /// Вызовов `glTexSubImage2D`.
    pub upload_calls: u64,
    /// Байт скопировано.
    pub copied_bytes: u64,
    /// Загрузок, отменённых как повтор текущего кадра.
    pub repeated_frames: u64,
    /// Загрузок, отклонённых `Busy`.
    pub busy_rejections: u64,
    /// Текстур выделено.
    pub texture_allocations: u64,
    /// Draw-вызовов видео.
    pub draw_calls: u64,
    /// Таблиц HDR → SDR посчитано и загружено (одна на смену передаточной функции или уровней,
    /// не на кадр).
    pub hdr_table_builds: u64,
}
