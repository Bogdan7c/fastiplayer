//! Субтитры плеера на GLES2 (сессия 08 плана `user/gles2-renderer/`): перевод `SubtitleOverlay`
//! растеризатора в слой оверлеев `gles2-video` по контракту второго рендера.
//!
//! Контракт тот же, что у wgpu-прохода (`render-wgpu-shell/src/subtitle_overlay/`, строгий паритет):
//! - вход — [`SubtitleOverlayFrame`] (overlay + картинка видео без полос + сдвиги групп) и видимая
//!   область видео; угол холста считает контракт ([`SubtitleOverlayFrame::canvas_origins`]);
//! - рисовать можно только внутри видимой области видео ∩ экран;
//! - тот же overlay (revision + те же пиксели каждой картинки) не загружается; сдвиг — только
//!   uniform-ы;
//! - исходы и счётчики названы как у wgpu ([`SubtitleOverlayOutcome`],
//!   [`SubtitleOverlayUploadStats`]), чтобы портированные тесты и диагностика сравнивались один к
//!   одному.
//!
//! Отличие одно: GL может отказать в памяти под атлас (у wgpu такой ошибки нет — он паникует).
//! Отказ загрузки не роняет кадр видео: субтитры этого overlay пропускаются
//! ([`SubtitleOverlayOutcome::SkippedUploadFailed`]), счётчик `failed_uploads` и предупреждение в
//! лог, новая попытка — со сменой overlay.

mod bitmap;
mod layer;

use gles2_egui::{ActiveFrame, Gles2Context};
use gles2_video::{
    BitmapOverlayRenderer, OverlayCanvasOrigins, OverlayDrawError, OverlayDrawOutcome,
    OverlayDrawRequest, OverlayLayerError, OverlayLayerOptions, OverlayPrepareError, PixelRect,
};
use render_core::RenderViewport;
use render_shell_api::SubtitleOverlayFrame;
use subtitle_raster::{OverlayRevision, SubtitleOverlay};
use thiserror::Error;

use self::bitmap::SubtitleOverlayBitmap;
pub use self::layer::SubtitleOverlayLayer;

/// Счётчики загрузок (имена и смысл — как у `render_wgpu_shell::SubtitleOverlayUploadStats`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubtitleOverlayUploadStats {
    /// Загрузок атласа масок текста.
    pub atlas_uploads: u64,
    /// Масок текста загружено.
    pub bitmap_uploads: u64,
    /// Выделений текстуры атласа масок (первое и каждый рост).
    pub atlas_recreations: u64,
    /// Подготовок, в которых маски текста пропущены: не влезли в атлас.
    pub skipped_overlays_too_large: u64,
    /// Смен uniform-ов (размер экрана, углы холста групп).
    pub uniform_uploads: u64,
    /// Загрузок атласа RGBA-картинок.
    pub picture_atlas_uploads: u64,
    /// RGBA-картинок загружено.
    pub picture_bitmap_uploads: u64,
    /// Выделений текстуры атласа картинок.
    pub picture_atlas_recreations: u64,
    /// RGBA-картинок пропущено: не влезли в атлас.
    pub skipped_picture_bitmaps_too_large: u64,
    /// Отказов загрузки (только GLES2: память атласа, нарушенный контракт картинки).
    pub failed_uploads: u64,
}

/// Что сделал проход субтитров (варианты wgpu + отказ загрузки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleOverlayOutcome {
    /// Рисовать нечего: субтитров нет, overlay пуст, видео не видно или координаты нечисловые.
    NothingToDraw,
    /// Все картинки overlay-а не влезли в атлас: ничего не нарисовано.
    SkippedTooLarge,
    /// Нарисовано.
    Drawn {
        /// Сколько картинок нарисовано.
        bitmaps: usize,
    },
    /// Загрузка этого overlay не удалась: субтитры пропущены до смены overlay.
    SkippedUploadFailed,
}

/// Отказ прохода, который вызывающий обязан показать как отказ кадра.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SubtitleOverlayError {
    /// Кадр чужого контекста (ошибка вызывающего).
    #[error("subtitle overlay used with a foreign GLES2 context")]
    ForeignContext,
    /// Отказ рисования (область вне координат GL, отказ GL).
    #[error("subtitle overlay draw: {0}")]
    Draw(#[source] OverlayDrawError),
}

/// Что сейчас подготовлено в слое: тот же overlay не загружается повторно.
struct UploadedOverlay {
    revision: OverlayRevision,
    /// Клоны картинок (держат буферы пикселей живыми — см. `shows_same_pixels_as`).
    bitmaps: Vec<SubtitleOverlayBitmap>,
    state: UploadState,
}

/// Итог подготовки этого overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UploadState {
    /// Подготовлен; сколько картинок рисуется (0 — все не влезли).
    Prepared { drawable_bitmaps: usize },
    /// Загрузка не удалась.
    Failed,
}

impl UploadedOverlay {
    /// Тот же overlay: та же ревизия и те же пиксели каждой картинки (ревизии одной не хватает:
    /// новый растеризатор начинает те же номера).
    fn is_same_as(&self, overlay: &SubtitleOverlay) -> bool {
        self.revision == overlay.revision()
            && self.bitmaps.len() == overlay.bitmaps().len()
            && self
                .bitmaps
                .iter()
                .zip(overlay.bitmaps())
                .all(|(uploaded, candidate)| uploaded.0.shows_same_pixels_as(candidate))
    }
}

/// Проход субтитров одного контекста GLES2.
pub struct Gles2SubtitleOverlayRenderer {
    layer: BitmapOverlayRenderer<SubtitleOverlayBitmap>,
    uploaded: Option<UploadedOverlay>,
    failed_uploads: u64,
}

impl Gles2SubtitleOverlayRenderer {
    /// Собрать слой оверлеев (программы; атласы — при первых субтитрах).
    ///
    /// # Errors
    /// [`OverlayLayerError`]: контекст недоступен, шейдер не собрался.
    pub fn new(
        context: &Gles2Context,
        options: OverlayLayerOptions,
    ) -> Result<Self, OverlayLayerError> {
        Ok(Self {
            layer: BitmapOverlayRenderer::new(context, options)?,
            uploaded: None,
            failed_uploads: 0,
        })
    }

    /// Счётчики загрузок.
    #[must_use]
    pub fn upload_stats(&self) -> SubtitleOverlayUploadStats {
        let stats = self.layer.stats();
        SubtitleOverlayUploadStats {
            atlas_uploads: stats.coverage.atlas_uploads,
            bitmap_uploads: stats.coverage.bitmap_uploads,
            atlas_recreations: stats.coverage.atlas_recreations,
            skipped_overlays_too_large: stats.coverage.skipped_prepares,
            uniform_uploads: stats.uniform_updates,
            picture_atlas_uploads: stats.picture.atlas_uploads,
            picture_bitmap_uploads: stats.picture.bitmap_uploads,
            picture_atlas_recreations: stats.picture.atlas_recreations,
            skipped_picture_bitmaps_too_large: stats.picture.skipped_bitmaps,
            failed_uploads: self.failed_uploads,
        }
    }

    /// Создан ли атлас RGBA-картинок (только тексты его не создают — как ленивый слой wgpu).
    #[must_use]
    pub fn has_picture_atlas(&self) -> bool {
        self.layer.stats().picture.atlas_side != 0
    }

    /// Подготовлен ли в слое именно этот overlay (те же пиксели всех картинок). Вопрос задаётся
    /// ДО `render` кадра: он показывает, изменятся ли пиксели субтитров относительно прошлого
    /// нарисованного кадра.
    #[must_use]
    pub fn has_prepared(&self, overlay: &SubtitleOverlay) -> bool {
        self.uploaded
            .as_ref()
            .is_some_and(|uploaded| uploaded.is_same_as(overlay))
    }

    /// Забыть загруженный overlay: следующий кадр загрузит субтитры заново (текстуры остаются).
    pub fn forget_uploaded(&mut self) {
        self.uploaded = None;
        self.layer.forget_prepared();
    }

    /// Нарисовать субтитры поверх уже нарисованного видео, только внутри `visible_area` ∩ экран.
    ///
    /// # Errors
    /// [`SubtitleOverlayError`]: чужой контекст, отказ рисования. Отказ загрузки — не ошибка
    /// кадра, а [`SubtitleOverlayOutcome::SkippedUploadFailed`].
    pub fn render(
        &mut self,
        frame: &ActiveFrame<'_>,
        overlay_frame: Option<&SubtitleOverlayFrame<'_>>,
        visible_area: RenderViewport,
    ) -> Result<SubtitleOverlayOutcome, SubtitleOverlayError> {
        let Some(overlay_frame) = overlay_frame else {
            return Ok(SubtitleOverlayOutcome::NothingToDraw);
        };
        let target = frame.size();
        let screen_area = RenderViewport::full_surface(target.width(), target.height());
        let (Some(origins), Some(scissor)) = (
            overlay_frame.canvas_origins(),
            visible_area.intersection(screen_area),
        ) else {
            return Ok(SubtitleOverlayOutcome::NothingToDraw);
        };
        if overlay_frame.overlay.bitmaps().is_empty() || scissor.is_empty() {
            return Ok(SubtitleOverlayOutcome::NothingToDraw);
        }
        match self.ensure_uploaded(frame, overlay_frame.overlay)? {
            UploadState::Failed => return Ok(SubtitleOverlayOutcome::SkippedUploadFailed),
            UploadState::Prepared {
                drawable_bitmaps: 0,
            } => return Ok(SubtitleOverlayOutcome::SkippedTooLarge),
            UploadState::Prepared { .. } => {}
        }
        let request = OverlayDrawRequest {
            origins: OverlayCanvasOrigins {
                main: origins.main,
                fixed: origins.fixed,
            },
            clip: PixelRect {
                x: scissor.x,
                y: scissor.y,
                width: scissor.width,
                height: scissor.height,
            },
        };
        let drawn = self
            .layer
            .draw(frame, &request)
            .map_err(|error| match error {
                OverlayDrawError::ForeignContext => SubtitleOverlayError::ForeignContext,
                other => SubtitleOverlayError::Draw(other),
            })?;
        Ok(match drawn {
            OverlayDrawOutcome::Drawn { bitmaps, .. } => SubtitleOverlayOutcome::Drawn { bitmaps },
            OverlayDrawOutcome::NothingPrepared => SubtitleOverlayOutcome::SkippedTooLarge,
            OverlayDrawOutcome::NothingVisible => SubtitleOverlayOutcome::NothingToDraw,
        })
    }

    /// Подготовить overlay, если он сменился; вернуть итог подготовки текущего overlay.
    fn ensure_uploaded(
        &mut self,
        frame: &ActiveFrame<'_>,
        overlay: &SubtitleOverlay,
    ) -> Result<UploadState, SubtitleOverlayError> {
        if let Some(uploaded) = &self.uploaded
            && uploaded.is_same_as(overlay)
        {
            return Ok(uploaded.state);
        }
        let bitmaps: Vec<SubtitleOverlayBitmap> = overlay
            .bitmaps()
            .iter()
            .cloned()
            .map(SubtitleOverlayBitmap)
            .collect();
        let state = match self.layer.prepare(frame, &bitmaps) {
            Ok(report) => UploadState::Prepared {
                drawable_bitmaps: report.drawable_bitmaps,
            },
            Err(OverlayPrepareError::ForeignContext) => {
                return Err(SubtitleOverlayError::ForeignContext);
            }
            Err(
                error @ (OverlayPrepareError::MalformedBitmap { .. } | OverlayPrepareError::Gl(_)),
            ) => {
                self.failed_uploads += 1;
                self.layer.forget_prepared();
                log::warn!(
                    "subtitle overlay revision {} was not uploaded, subtitles are skipped until \
                     it changes: {error}",
                    overlay.revision().value()
                );
                UploadState::Failed
            }
        };
        self.uploaded = Some(UploadedOverlay {
            revision: overlay.revision(),
            bitmaps,
            state,
        });
        Ok(state)
    }

    /// Удалить GL-объекты.
    ///
    /// # Errors
    /// [`OverlayLayerError::ForeignContext`] — контекст чужой.
    pub fn destroy(self, context: &Gles2Context) -> Result<(), OverlayLayerError> {
        self.layer.destroy(context)
    }
}
