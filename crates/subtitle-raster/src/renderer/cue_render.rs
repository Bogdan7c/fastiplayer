//! Отрисовка одной реплики целиком: стиль -> текст -> плашка -> готовый список картинок.

use subtitle_core::SubtitleTextCue;

use crate::appearance::{CollisionDirection, SubtitleAppearance, cue_style};
use crate::face_cache::FaceCache;
use crate::fonts::{SubtitleFontLibrary, TextLanguagePreference};
use crate::overlay::{CanvasSize, SubtitleBitmap};
use crate::plate::{PlateError, build_plate};
use crate::render_diagnostics::RenderDiagnostics;
use crate::text_renderer::{TextRenderError, render_cue_text};

/// Условная стоимость записи кэша сверх масок (заголовки картинок, вектор): без неё кэш
/// пустых и крошечных реплик рос бы без учёта в байтах.
const CUE_ENTRY_OVERHEAD_BYTES: usize = 256;

/// Почему реплику не удалось нарисовать.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub(crate) enum CueRenderError {
    /// Сбой текстового конвейера.
    #[error("text rendering failed: {0}")]
    Text(#[from] TextRenderError),
    /// Сбой построения плашки.
    #[error("plate rendering failed: {0}")]
    Plate(#[from] PlateError),
}

/// Прямоугольник в пикселях холста (правый и нижний края не включены).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PixelRect {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
}

/// Готовая реплика: картинки на положении «без столкновений» и данные для их разрешения.
#[derive(Debug)]
pub(crate) struct RasterizedCue {
    /// Картинки в порядке отрисовки (первой идёт плашка).
    pub(crate) bitmaps: Vec<SubtitleBitmap>,
    /// Объединённая рамка картинок; `None` — реплика ничего не рисует.
    pub(crate) extent: Option<PixelRect>,
    /// Куда сдвигать при столкновении.
    pub(crate) collision: CollisionDirection,
    /// Что пропущено при отрисовке (учитывается один раз, при растеризации).
    pub(crate) diagnostics: RenderDiagnostics,
}

impl RasterizedCue {
    /// Размер в кэше: байты масок плюс условная стоимость записи.
    pub(crate) fn cache_bytes(&self) -> usize {
        let masks: usize = self
            .bitmaps
            .iter()
            .map(|bitmap| bitmap.coverage().len())
            .sum();
        masks.saturating_add(CUE_ENTRY_OVERHEAD_BYTES)
    }
}

/// Сигнатура рисовальщика одной реплики.
///
/// Растеризатор вызывает рисование через этот тип, чтобы тесты могли подставить рисовальщика,
/// который падает на заданной реплике: настоящих дешёвых путей ошибки в рисовании почти нет
/// (ошибка возможна лишь на гигантских холстах), а политику «битая реплика не роняет
/// остальные» надо проверять.
pub(crate) type CuePainter = fn(
    &mut SubtitleFontLibrary,
    &mut FaceCache,
    &SubtitleTextCue,
    &SubtitleAppearance,
    CanvasSize,
    TextLanguagePreference,
) -> Result<RasterizedCue, CueRenderError>;

/// Рисует реплику стилем пользователя на холсте.
pub(crate) fn rasterize_cue(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    cue: &SubtitleTextCue,
    appearance: &SubtitleAppearance,
    canvas: CanvasSize,
    language: TextLanguagePreference,
) -> Result<RasterizedCue, CueRenderError> {
    note_unsupported_span_backgrounds(cue);
    let style = cue_style(appearance, &cue.placement, canvas);
    let rendered = render_cue_text(fonts, faces, &cue.text, &style.event, canvas, language)?;

    let mut bitmaps = Vec::with_capacity(rendered.images.len() + 1);
    // Плашка идёт первой: она лежит под текстом.
    if let Some(plate_style) = style.plate
        && let Some(plate) = build_plate(&rendered.lines, &plate_style, canvas)?
    {
        bitmaps.push(plate);
    }
    bitmaps.extend(rendered.images);
    let extent = union_extent(&bitmaps);
    Ok(RasterizedCue {
        bitmaps,
        extent,
        collision: style.collision,
        diagnostics: rendered.diagnostics,
    })
}

/// Фон отдельного спана (`SpanStyle.background`) пока не рисуется: только отладочная запись
/// (авторские фоны — задача бэклога/сессии 14).
fn note_unsupported_span_backgrounds(cue: &SubtitleTextCue) {
    if cue
        .text
        .spans()
        .iter()
        .any(|span| span.style.background.is_some())
    {
        tracing::debug!("span background colors are not supported yet and are ignored");
    }
}

/// Рамка, охватывающая все картинки.
fn union_extent(bitmaps: &[SubtitleBitmap]) -> Option<PixelRect> {
    bitmaps
        .iter()
        .map(|bitmap| {
            let origin = bitmap.origin();
            PixelRect {
                left: origin.x,
                top: origin.y,
                right: origin.x.saturating_add_unsigned(bitmap.width()),
                bottom: origin.y.saturating_add_unsigned(bitmap.height()),
            }
        })
        .reduce(|a, b| PixelRect {
            left: a.left.min(b.left),
            top: a.top.min(b.top),
            right: a.right.max(b.right),
            bottom: a.bottom.max(b.bottom),
        })
}
