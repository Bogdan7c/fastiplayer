//! Словарь импорта DMA-BUF видеослоя: исходы, отчёт и ошибки.
//!
//! Отдельно от логики ([`crate::dma_buf_import`]) по правилам проекта: исходы «показан из кэша»,
//! «нет в кэше», «импортирован», «повтор» и отказы разных причин — разные варианты, а не `bool`.

use thiserror::Error;

use gles2_egui::{DmaBufTextureAttached, DmaBufTextureError, GlAccessError};

use crate::dma_buf_frame::DmaBufPlaneRole;
use crate::gl_calls::VideoGlError;
use crate::planes::PlaneRole;

/// Итог попытки показать кадр по паспорту буфера без импорта (без GL и без fd).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedDmaBufFrame {
    /// Буфер уже импортирован: кадр стал текущим, ничего не импортировано.
    Shown,
    /// Этот кадр (ключ) уже текущий: ничего не изменилось.
    AlreadyCurrent,
    /// Буфера нет в кэше: вызывающему нужно дать дескрипторы и позвать
    /// [`crate::VideoLayerRenderer::import_dma_buf_frame`].
    NotCached,
}

/// Что сделал импорт.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaBufImportOutcome {
    /// Буфер импортирован заново: две текстуры на `EGLImage`, кадр стал текущим.
    Imported(DmaBufImportReport),
    /// Буфер уже лежал в кэше (вызывающий не спросил кэш заранее): импорта нет, кадр текущий.
    /// Не считается попаданием кэша: счётчик `hits` — это показы через
    /// [`crate::VideoLayerRenderer::show_cached_dma_buf_frame`].
    ReusedCachedImport,
    /// Этот кадр (ключ) уже текущий: ничего не изменилось.
    AlreadyCurrent,
}

/// Что сделал импорт планарного кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanarDmaBufImportOutcome {
    /// Буфер импортирован заново: три текстуры на `EGLImage`, кадр стал текущим.
    Imported(PlanarDmaBufImportReport),
    /// Буфер уже лежал в кэше (вызывающий не спросил кэш заранее): импорта нет, кадр текущий.
    ReusedCachedImport,
    /// Этот кадр (ключ) уже текущий: ничего не изменилось.
    AlreadyCurrent,
}

/// Отчёт об импорте трёх плоскостей планарного кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanarDmaBufImportReport {
    /// Привязка яркости Y (`R8`).
    pub luma: DmaBufTextureAttached,
    /// Привязка цветности U (`R8`).
    pub chroma_u: DmaBufTextureAttached,
    /// Привязка цветности V (`R8`).
    pub chroma_v: DmaBufTextureAttached,
}

/// Отчёт об импорте двух плоскостей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaBufImportReport {
    /// Привязка яркости (`R8`).
    pub luma: DmaBufTextureAttached,
    /// Привязка цветности (`GR88`).
    pub chroma: DmaBufTextureAttached,
}

/// Отказ импорта кадра DMA-BUF. Честный отказ: копии вместо импорта нет никогда.
///
/// После любой ошибки в кэш ничего не добавлено, созданные текстуры удалены, а кадр, который
/// был текущим до вызова, остаётся текущим — кроме случая, когда его текстуры вытеснила
/// подготовка места под импорт (смена формата потока или LRU): тогда текущего кадра нет и
/// рисование даёт `ClearedWithoutFrame`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VideoImportError {
    /// Вызов с контекстом, которому видеослой не принадлежит.
    #[error("video layer used with a foreign GLES2 context")]
    ForeignContext,
    /// Контекст недоступен.
    #[error("GLES2 context unavailable: {0}")]
    GlAccess(#[source] GlAccessError),
    /// Плоскость текстуры больше, чем разрешает драйвер.
    #[error("{plane:?} plane needs a {width}x{height} texture, driver limit is {max}")]
    PlaneTooLarge {
        /// Плоскость.
        plane: DmaBufPlaneRole,
        /// Ширина.
        width: u32,
        /// Высота.
        height: u32,
        /// `GL_MAX_TEXTURE_SIZE`.
        max: u32,
    },
    /// Плоскость планарного кадра больше, чем разрешает драйвер.
    #[error("{role:?} plane needs a {width}x{height} texture, driver limit is {max}")]
    PlanarPlaneTooLarge {
        /// Плоскость.
        role: PlaneRole,
        /// Ширина.
        width: u32,
        /// Высота.
        height: u32,
        /// `GL_MAX_TEXTURE_SIZE`.
        max: u32,
    },
    /// Драйвер не принял плоскость планарного кадра (причина от `gles2-egui` сохранена).
    #[error("{role:?} plane import failed: {source}")]
    PlanarAttach {
        /// Плоскость.
        role: PlaneRole,
        /// Причина от `gles2-egui`.
        #[source]
        source: DmaBufTextureError,
    },
    /// Драйвер не создал объект текстуры.
    #[error("video layer GL: {0}")]
    Gl(#[source] VideoGlError),
    /// Драйвер не принял плоскость. Различия сохранены: формат не читается как `sampler2D`,
    /// нет расширений, отказ `eglCreateImage`, контекст недоступен — всё в `source`.
    #[error("{plane:?} plane import failed: {source}")]
    Attach {
        /// Плоскость.
        plane: DmaBufPlaneRole,
        /// Причина от `gles2-egui`.
        #[source]
        source: DmaBufTextureError,
    },
}
