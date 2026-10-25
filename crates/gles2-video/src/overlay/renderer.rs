//! [`BitmapOverlayRenderer`]: подготовка (атласы, вершины) и рисование оверлея поверх цели.

use gles2_egui::{ActiveFrame, ContextId, Gles2Context};

use crate::geometry::{PixelRect, to_gl_window_rect};
use crate::gl_calls::VideoGl;
use crate::gl_calls::overlay::AtlasTextureFormat;
use crate::layer_api::PlaneFormatPreference;
use crate::program::ShaderPrecision;

use super::OverlayBitmap;
use super::api::{
    AtlasSideLimit, OverlayDrawError, OverlayDrawOutcome, OverlayDrawRequest, OverlayLayerError,
    OverlayLayerOptions, OverlayLayerStats, OverlayPrepareError, OverlayPrepareReport,
    OverlayRowUploadPreference,
};
use super::layer::{AtlasLayer, LayerPrepareInput, checked_rows};
use super::program::{OverlayPrograms, OverlayUniformValues};
use super::runs::{DrawRun, OverlayLayerKind, partition_overlay};
use super::upload_calls::RowStepSupport;
use super::vertices::VERTEX_LAYOUT;

/// Слой bitmap-оверлеев одного контекста GLES2.
///
/// Порядок работы вызывающего (как у wgpu-прохода субтитров):
/// 1. [`Self::prepare`] — только когда набор картинок сменился (вызывающий знает, когда: у плеера
///    — номер ревизии оверлея растеризатора). Повтор с теми же пикселями ничего не загружает, но
///    обновляет вершины;
/// 2. [`Self::draw`] — каждый кадр, внутри `FrameLayers::draw_overlays`. Сдвиг групп и размер цели
///    — только uniform-ы.
pub struct BitmapOverlayRenderer<B> {
    owner: ContextId,
    programs: OverlayPrograms,
    coverage: AtlasLayer<B>,
    picture: AtlasLayer<B>,
    /// Серии последнего успешного `prepare` (пусто — рисовать нечего).
    runs: Vec<DrawRun>,
    drawable_bitmaps: usize,
    max_atlas_side: u32,
    row_step: RowStepSupport,
    last_uniforms: Option<OverlayUniformValues>,
    uniform_updates: u64,
    draw_calls: u64,
    destroyed: bool,
}

impl<B: OverlayBitmap> BitmapOverlayRenderer<B> {
    /// Собрать программы. Атласы и буферы вершин создаются при первой картинке своего вида.
    ///
    /// # Errors
    /// [`OverlayLayerError`]: контекст недоступен, шейдер не собрался.
    pub fn new(
        context: &Gles2Context,
        options: OverlayLayerOptions,
    ) -> Result<Self, OverlayLayerError> {
        let capabilities = context.capabilities();
        let precision = ShaderPrecision::for_capabilities(capabilities);
        let coverage_format = match options.coverage_format {
            PlaneFormatPreference::BestAvailable
                if capabilities.gl_extensions().contains("GL_EXT_texture_rg") =>
            {
                AtlasTextureFormat::CoverageRedExt
            }
            PlaneFormatPreference::BestAvailable | PlaneFormatPreference::LuminanceOnly => {
                AtlasTextureFormat::CoverageLuminance
            }
        };
        let row_step = match options.row_upload {
            OverlayRowUploadPreference::BestAvailable
                if capabilities.tiers().conveniences.unpack_subimage =>
            {
                RowStepSupport::UnpackRowLength
            }
            OverlayRowUploadPreference::BestAvailable | OverlayRowUploadPreference::RowByRow => {
                RowStepSupport::RowByRow
            }
        };
        let max_atlas_side = max_atlas_side(
            capabilities.max_texture_size(),
            precision,
            options.atlas_side_limit,
        );
        let programs = OverlayPrograms::build(context, precision)?;
        Ok(Self {
            owner: context.id(),
            programs,
            coverage: AtlasLayer::new(coverage_format),
            picture: AtlasLayer::new(AtlasTextureFormat::PremultipliedRgba),
            runs: Vec::new(),
            drawable_bitmaps: 0,
            max_atlas_side,
            row_step,
            last_uniforms: None,
            uniform_updates: 0,
            draw_calls: 0,
            destroyed: false,
        })
    }

    /// Предел стороны атласа на этом драйвере (картинки крупнее пропускаются).
    #[must_use]
    pub const fn max_atlas_side(&self) -> u32 {
        self.max_atlas_side
    }

    /// Счётчики.
    #[must_use]
    pub fn stats(&self) -> OverlayLayerStats {
        OverlayLayerStats {
            coverage: self.coverage.stats(),
            picture: self.picture.stats(),
            uniform_updates: self.uniform_updates,
            draw_calls: self.draw_calls,
        }
    }

    /// Подготовить набор картинок к рисованию (в порядке списка, первая — самая нижняя).
    ///
    /// Картинка со стороной больше [`Self::max_atlas_side`] пропускается со счётчиком, остальные
    /// готовятся. Каждый вид загружает атлас, только если его пиксели сменились.
    ///
    /// # Errors
    /// [`OverlayPrepareError`]: чужой контекст, пиксели картинки не сходятся с её размером
    /// (проверка всего списка до GL — атласы не тронуты), отказ GL. После ошибки слой ничего не
    /// рисует до следующего успешного `prepare`.
    pub fn prepare(
        &mut self,
        frame: &ActiveFrame<'_>,
        bitmaps: &[B],
    ) -> Result<OverlayPrepareReport, OverlayPrepareError> {
        if frame.context_id() != self.owner {
            return Err(OverlayPrepareError::ForeignContext);
        }
        for (index, bitmap) in bitmaps.iter().enumerate() {
            checked_rows(index, bitmap.pixels(), bitmap)?;
        }
        // Прежние серии больше не действуют: при ошибке ниже рисовать нечего.
        self.runs.clear();
        self.drawable_bitmaps = 0;
        let gl = VideoGl::for_active_frame(frame);
        let partition = partition_overlay(bitmaps, self.max_atlas_side);
        let coverage = self.coverage.prepare(
            gl,
            &LayerPrepareInput {
                bitmaps: &partition.coverage,
                dropped_by_size: partition.dropped_coverage,
                max_side: self.max_atlas_side,
                row_step: self.row_step,
            },
        )?;
        let picture = self.picture.prepare(
            gl,
            &LayerPrepareInput {
                bitmaps: &partition.pictures,
                dropped_by_size: partition.dropped_pictures,
                max_side: self.max_atlas_side,
                row_step: self.row_step,
            },
        )?;
        let drawable_kind = |kind: OverlayLayerKind| {
            let outcome = match kind {
                OverlayLayerKind::Coverage => coverage,
                OverlayLayerKind::Picture => picture,
            };
            !matches!(outcome, super::OverlayAtlasOutcome::DoesNotFit { .. })
        };
        self.runs = partition
            .runs
            .into_iter()
            .filter(|run| drawable_kind(run.kind))
            .collect();
        self.drawable_bitmaps = self.runs.iter().map(|run| run.bitmap_count).sum();
        Ok(OverlayPrepareReport {
            coverage,
            picture,
            larger_than_atlas_limit: partition.dropped_coverage + partition.dropped_pictures,
            drawable_bitmaps: self.drawable_bitmaps,
        })
    }

    /// Забыть подготовленный набор: до следующего `prepare` рисовать нечего, следующий `prepare`
    /// загрузит пиксели заново (текстуры остаются).
    pub fn forget_prepared(&mut self) {
        self.runs.clear();
        self.drawable_bitmaps = 0;
        self.coverage.forget_contents();
        self.picture.forget_contents();
    }

    /// Нарисовать подготовленный набор поверх цели кадра, только внутри `request.clip`.
    ///
    /// Смешивание `ONE, ONE_MINUS_SRC_ALPHA`; после прохода смешивание и scissor выключены, VAO
    /// не тронут (контракт `FrameLayers`).
    ///
    /// # Errors
    /// [`OverlayDrawError`]: чужой контекст, область вне координат GL, отказ GL. Все проверки
    /// области — до записи в цель.
    pub fn draw(
        &mut self,
        frame: &ActiveFrame<'_>,
        request: &OverlayDrawRequest,
    ) -> Result<OverlayDrawOutcome, OverlayDrawError> {
        if frame.context_id() != self.owner {
            return Err(OverlayDrawError::ForeignContext);
        }
        if self.runs.is_empty() {
            return Ok(OverlayDrawOutcome::NothingPrepared);
        }
        let target = frame.size();
        let target_rect = PixelRect {
            x: 0,
            y: 0,
            width: target.width(),
            height: target.height(),
        };
        let Some(clip) = request.clip.intersection(target_rect) else {
            return Ok(OverlayDrawOutcome::NothingVisible);
        };
        let scissor = to_gl_window_rect(clip, target.height())?;
        let uniforms = OverlayUniformValues {
            screen_size: [target.width() as f32, target.height() as f32],
            origins: request.origins,
        };
        if self.last_uniforms != Some(uniforms) {
            self.uniform_updates += 1;
            self.last_uniforms = Some(uniforms);
        }
        let gl = VideoGl::for_active_frame(frame);
        gl.set_premultiplied_overlay_state(scissor);
        let drawn = self.draw_runs(gl, uniforms);
        gl.reset_overlay_state();
        let draw_calls = drawn?;
        self.draw_calls += draw_calls as u64;
        Ok(OverlayDrawOutcome::Drawn {
            bitmaps: self.drawable_bitmaps,
            draw_calls,
        })
    }

    /// Серии по порядку: каждая — программа своего вида, свой атлас, свой буфер, один draw.
    fn draw_runs(
        &mut self,
        gl: VideoGl<'_>,
        uniforms: OverlayUniformValues,
    ) -> Result<usize, OverlayDrawError> {
        let mut draw_calls = 0;
        for run in &self.runs {
            let layer = match run.kind {
                OverlayLayerKind::Coverage => &self.coverage,
                OverlayLayerKind::Picture => &self.picture,
            };
            // Ресурсы гарантирует успешный `prepare` для серий в плане; отсутствие — нарушение
            // инварианта, которое безопаснее пропустить, чем уронить кадр видео.
            let (Some(resources), Some(vertices)) = (layer.draw_resources(), run.vertex_range())
            else {
                log::error!(
                    "gles2-video: overlay run {:?} is planned, but its GL resources are missing",
                    run.kind
                );
                continue;
            };
            let program = self.programs.for_kind(run.kind);
            program.activate(gl, uniforms);
            let attributes = program.attributes();
            gl.bind_atlas_texture(resources.texture);
            gl.bind_overlay_vertices(resources.vertex_buffer, attributes, VERTEX_LAYOUT)
                .map_err(OverlayDrawError::Gl)?;
            let drawn = gl.draw_overlay_triangles(vertices, resources.vertex_count);
            gl.unbind_overlay_vertices(attributes);
            drawn.map_err(OverlayDrawError::Gl)?;
            draw_calls += 1;
        }
        Ok(draw_calls)
    }

    /// Удалить GL-объекты слоя.
    ///
    /// # Errors
    /// [`OverlayLayerError::ForeignContext`] — контекст чужой (объекты не тронуты). Недоступный
    /// контекст не ошибка: объекты умрут вместе с ним, это пишется в лог.
    pub fn destroy(mut self, context: &Gles2Context) -> Result<(), OverlayLayerError> {
        if context.id() != self.owner {
            return Err(OverlayLayerError::ForeignContext);
        }
        self.destroyed = true;
        match VideoGl::for_context(context) {
            Ok(gl) => {
                self.coverage.delete(gl);
                self.picture.delete(gl);
                self.programs.delete(context);
            }
            Err(error) => log::warn!(
                "gles2-video: context unavailable on overlay destroy ({error}); GL objects die \
                 with it"
            ),
        }
        Ok(())
    }
}

impl<B> Drop for BitmapOverlayRenderer<B> {
    fn drop(&mut self) {
        if !self.destroyed {
            log::warn!(
                "overlay layer dropped without destroy(): its programs{} live until the GLES2 \
                 context is destroyed",
                if self.coverage.has_gl_objects() || self.picture.has_gl_objects() {
                    ", atlases and buffers"
                } else {
                    ""
                }
            );
        }
    }
}

/// Предел стороны атласа: драйвер, точность фрагментного шейдера, ограничение вызывающего.
fn max_atlas_side(
    driver_max_texture_size: u32,
    precision: ShaderPrecision,
    limit: AtlasSideLimit,
) -> u32 {
    let precision_limit = match precision {
        ShaderPrecision::High => u32::MAX,
        ShaderPrecision::Medium => AtlasSideLimit::MEDIUM_PRECISION_MAX_SIDE,
    };
    let caller_limit = match limit {
        AtlasSideLimit::DriverMaximum => u32::MAX,
        AtlasSideLimit::AtMost(side) => side,
    };
    driver_max_texture_size
        .min(precision_limit)
        .min(caller_limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_limit_takes_the_tightest_of_driver_precision_and_caller() {
        let driver = 8192;
        assert_eq!(
            max_atlas_side(driver, ShaderPrecision::High, AtlasSideLimit::DriverMaximum),
            8192
        );
        assert_eq!(
            max_atlas_side(
                driver,
                ShaderPrecision::Medium,
                AtlasSideLimit::DriverMaximum
            ),
            256,
            "mediump адресует только маленький атлас"
        );
        assert_eq!(
            max_atlas_side(driver, ShaderPrecision::High, AtlasSideLimit::AtMost(300)),
            300
        );
        assert_eq!(
            max_atlas_side(2048, ShaderPrecision::High, AtlasSideLimit::AtMost(4096)),
            2048,
            "вызывающий не поднимает предел драйвера"
        );
    }
}
