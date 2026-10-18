//! Растровые реплики (PGS) в растеризаторе: кэш, счётчики, сборка картинок кадра.
//!
//! Рисует сам `crate::picture`; здесь только то, чем владеет растеризатор: запись в кэше
//! готовых реплик, учёт отказов и порядок картинок в кадре.
//!
//! Ключ кэша: `(CueKey, холст)` плюс вид [`CueLook::Picture`]. Пользовательский стиль и язык
//! в ключ НЕ входят (картинка от них не зависит), поэтому смена стиля картинки не
//! перерисовывает. Бюджет тот же, что у текстовых реплик (`cue_bitmaps_bytes`): отдельного
//! лимита нет. Раз `CueKey` и холст те же, картинка берётся из кэша тем же `Arc` буфера, и
//! `detect_overlay_change` видит `Unchanged`.

use std::sync::Arc;
use std::sync::OnceLock;

use subtitle_core::SubtitlePictureCue;

use super::cue_render::RasterizedCue;
use super::request::{CueKey, RenderableCue};
use super::{CueCacheKey, CueLook, SubtitleRasterizer};
use crate::appearance::CollisionDirection;
use crate::clip::CueClip;
use crate::fonts::TextLanguagePreference;
use crate::overlay::{CanvasSize, SubtitleBitmap};
use crate::picture::{PictureRenderError, rasterize_picture};
use crate::render_diagnostics::RenderDiagnostics;

impl RasterizedCue {
    /// Запись кэша для растровой реплики. Столкновений, клипа и караоке у неё нет, поэтому
    /// соответствующие поля нейтральны; `bitmap = None` - реплика ничего не рисует на холсте.
    fn from_picture(bitmap: Option<SubtitleBitmap>) -> Self {
        let bitmaps: Vec<SubtitleBitmap> = bitmap.into_iter().collect();
        Self {
            extent: super::cue_render::union_extent(&bitmaps),
            bitmaps,
            // Растровые реплики столкновениями не двигаются; значение не читается.
            collision: CollisionDirection::Up,
            ass_collision: None,
            diagnostics: RenderDiagnostics::default(),
            clip: CueClip::NONE,
            clip_animation: None,
            karaoke: None,
            clipped_bitmaps: OnceLock::new(),
        }
    }
}

impl SubtitleRasterizer {
    /// Картинки растровых реплик кадра по порядку запроса (первая - самая нижняя).
    pub(super) fn picture_bitmaps(
        &mut self,
        cues: &[&RenderableCue<'_>],
        canvas: CanvasSize,
    ) -> Vec<SubtitleBitmap> {
        let mut bitmaps = Vec::new();
        for cue in cues {
            if let super::CueContent::Picture(picture) = cue.content
                && let Some(rasterized) = self.picture_for(cue.key, picture, canvas)
            {
                bitmaps.extend(rasterized.bitmaps.iter().cloned());
            }
        }
        bitmaps
    }

    /// Готовая растровая реплика из кэша или свежая; `None` - реплику не удалось нарисовать
    /// (причина учтена в счётчиках, остальные реплики кадра не страдают).
    fn picture_for(
        &mut self,
        key: CueKey,
        picture: &SubtitlePictureCue,
        canvas: CanvasSize,
    ) -> Option<Arc<RasterizedCue>> {
        let cache_key = CueCacheKey {
            cue: key,
            canvas,
            look: CueLook::Picture,
            // Язык на картинку не влияет: фиксируем, чтобы смена языка не плодила копии.
            text_language: TextLanguagePreference::unknown(),
        };
        if let Some(hit) = self.cue_cache.get(&cache_key) {
            return Some(hit);
        }
        match rasterize_picture(picture, canvas, self.limits.input.max_canvas_side) {
            Ok(raster) => {
                self.counters.rasterized_cues += 1;
                if raster.damaged_data {
                    self.counters.damaged_pictures += 1;
                }
                let bitmap = raster.bitmap.map(|bitmap| bitmap.with_owner(key));
                let rasterized = RasterizedCue::from_picture(bitmap);
                let bytes = rasterized.cache_bytes();
                let shared = Arc::new(rasterized);
                self.cue_cache.insert(cache_key, Arc::clone(&shared), bytes);
                Some(shared)
            }
            Err(error) => {
                match error {
                    PictureRenderError::OverLimit { .. } => {
                        self.counters.rejected_pictures_too_large += 1;
                    }
                    PictureRenderError::Undecodable(_) | PictureRenderError::InconsistentImage => {
                        self.counters.failed_pictures += 1;
                    }
                }
                // Содержимое картинки в журнал не пишем: только метаданные.
                tracing::warn!(
                    %error,
                    source = key.source().get(),
                    event = key.event().get(),
                    "picture subtitle cue skipped"
                );
                None
            }
        }
    }
}
