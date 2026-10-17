//! Отрисовка одной реплики целиком: стиль -> текст -> плашка -> готовый список картинок.

use std::sync::OnceLock;

use subtitle_core::SubtitleTextCue;

use crate::appearance::{CollisionDirection, SubtitleAppearance, cue_style};
use crate::bitmap::BitmapError;
use crate::clip::CueClip;
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
    /// Сбой растеризации маски векторного клипа.
    #[error("clip rendering failed: {0}")]
    Clip(#[from] BitmapError),
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
    /// Данные столкновений по правилам libass (рамка по метрикам текста); только у ASS.
    pub(crate) ass_collision: Option<AssCollisionRect>,
    /// Что пропущено при отрисовке (учитывается один раз, при растеризации).
    pub(crate) diagnostics: RenderDiagnostics,
    /// Клип реплики (`\clip`/`\iclip`), не зависящий от времени. `bitmaps` хранит картинки БЕЗ
    /// клипа; клип накладывает стадия [`crate::clip`] при сборке кадра (см. [`Self::final_bitmaps`]).
    pub(crate) clip: CueClip,
    /// Результат наложения клипа на `bitmaps`: считается один раз при первой сборке кадра и
    /// живёт в записи кэша, чтобы маски клипнутых картинок были общими между кадрами.
    pub(crate) clipped_bitmaps: OnceLock<Vec<SubtitleBitmap>>,
}

/// Рамка реплики ASS для разрешения столкновений (libass: `EventImages`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AssCollisionRect {
    /// Рамка по метрикам текста вместе с отступами обводки.
    pub(crate) rect: PixelRect,
    /// Ищет ли реплика свободное место (у реплик с `\pos` и т. п. нет).
    pub(crate) detect_collisions: bool,
}

impl RasterizedCue {
    /// Размер в кэше: байты масок плюс условная стоимость записи.
    ///
    /// У реплики с клипом добавляется маска клипа и ещё раз размер масок: столько же (не
    /// больше) займёт запомненный результат клипа ([`Self::final_bitmaps`]).
    pub(crate) fn cache_bytes(&self) -> usize {
        let masks: usize = self
            .bitmaps
            .iter()
            .map(|bitmap| bitmap.coverage().len())
            .sum();
        let clipped_copy = if self.clip.is_none() { 0 } else { masks };
        masks
            .saturating_add(clipped_copy)
            .saturating_add(self.clip.mask_bytes())
            .saturating_add(CUE_ENTRY_OVERHEAD_BYTES)
    }

    /// Картинки реплики, как они идут в кадр: с наложенным клипом.
    ///
    /// Без клипа это сами `bitmaps` (никаких копий). С клипом стадия [`crate::clip`] считается
    /// один раз и запоминается: клип не зависит от времени, а общие маски между кадрами нужны
    /// детектору изменений (`Unchanged`/`Moved` вместо `Replaced`).
    pub(crate) fn final_bitmaps(&self) -> &[SubtitleBitmap] {
        if self.clip.is_none() {
            return &self.bitmaps;
        }
        self.clipped_bitmaps
            .get_or_init(|| self.clip.apply(&self.bitmaps))
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
        ass_collision: None,
        diagnostics: rendered.diagnostics,
        clip: CueClip::NONE,
        clipped_bitmaps: OnceLock::new(),
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
pub(super) fn union_extent(bitmaps: &[SubtitleBitmap]) -> Option<PixelRect> {
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
