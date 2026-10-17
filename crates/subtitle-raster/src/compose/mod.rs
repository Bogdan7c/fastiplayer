// Порт из libass 0.17.5: ass_render.c (render_and_combine_glyphs, render_text).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
//
// Permission to use, copy, modify, and distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
// ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
// OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//
// Rust port © Fastiplayer contributors.

//! Композиция реплики в список картинок.
//!
//! Порядок как в libass: сначала глифы переводятся в координаты кадра и растеризуются
//! (по участкам единого стиля), битмапы участка объединяются в три слоя, затем слои
//! выдаются списком «все тени, все обводки, все заливки». Размытие (`\blur`, `\be`) — часть
//! объединения слоёв участка (`run::composite_run`), параметры фильтров — в `filter`. Для `BorderStyle = 4` в начало
//! списка добавляется подложка, а тени не выдаются.

mod filter;
mod glyph_bitmaps;
mod images;
mod rotation;
mod run;

use subtitle_core::SubtitleColor;

use crate::bitmap::BitmapError;
use crate::blur::{BitmapSideLimit, BlurScratch};
use crate::event_style::{BorderStyle, EventStyle};
use crate::fixed_point::{d6_to_double, double_to_d6};
use crate::glyph_cache::{GlyphCaches, PlacedMask};
use crate::glyph_info::TextCluster;
use crate::glyph_transform::RASTERIZER_PRECISION;
use crate::karaoke::{
    ImageRole, KaraokeEffect, KaraokeRunRule, KaraokeStage, KaraokeStageBuilder, KaraokeWord,
    LeftmostTracker, RuleInputs,
};
use crate::layout::TextLayout;
use crate::overlay::{CanvasSize, SubtitleBitmap, SubtitleBitmapError, SubtitleBitmapKind};
use crate::raster::Rasterizer;
use crate::source_text::SpanLook;
use filter::RunFilter;
use glyph_bitmaps::{
    GlyphBitmapRequest, GlyphBitmaps, RunPhase, build_glyph_bitmaps, glyph_matrices,
};
use run::{BlurEnv, RunBitmaps, composite_run};

/// Почему композиция не удалась.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ComposeError {
    /// Сбой растеризации или операций над битмапами.
    #[error(transparent)]
    Bitmap(#[from] BitmapError),
    /// Итоговая картинка нарушает контракт выхода.
    #[error(transparent)]
    Image(#[from] SubtitleBitmapError),
    /// Контур глифа вне допустимого диапазона libass.
    #[error(transparent)]
    Outline(#[from] crate::outline::OutlineError),
}

/// Входные данные композиции.
pub(crate) struct ComposeInput<'a> {
    /// Внешние виды символов: цвета, обводка, тень, подложка каждого участка.
    pub(crate) looks: &'a [SpanLook],
    /// Стиль реплики (то, что общее для всей реплики: подложка `BorderStyle = 4`).
    pub(crate) style: &'a EventStyle,
    /// Результат раскладки.
    pub(crate) layout: &'a TextLayout,
    /// Холст.
    pub(crate) canvas: CanvasSize,
    /// Растяжение по горизонтали `par_scale_x` libass (1.0 — пропорции хранения и кадра равны).
    pub(crate) pixel_aspect_x: f64,
    /// Наибольшая сторона битмапа глифа в пикселях (`RasterInputLimits::max_canvas_side`).
    pub(crate) max_bitmap_side: u32,
}

/// Четыре цвета участка (libass: `c[0..4]`).
#[derive(Debug, Clone, Copy)]
struct RunColors {
    primary: SubtitleColor,
    /// Вторичный цвет: второй цвет заливки караоке.
    secondary: SubtitleColor,
    outline: SubtitleColor,
    back: SubtitleColor,
}

/// Слой вывода: какой вид картинки, откуда взять битмап участка и каким цветом его красить.
struct Layer {
    kind: SubtitleBitmapKind,
    bitmap_of: fn(&RunBitmaps) -> Option<&PlacedMask>,
    color_of: fn(&RunColors) -> SubtitleColor,
    /// Роль картинки слоя для караоке; аргумент — индекс правила участка (если оно есть).
    role_of: fn(Option<usize>) -> ImageRole,
}

/// Караоке участка, пока собираются его глифы.
struct RunKaraoke {
    /// Слово караоке, которому принадлежит участок.
    word: KaraokeWord,
    /// Позиция первого глифа участка в предварительной раскладке, 26.6.
    first_origin_x: i32,
    /// Самая левая точка контуров участка на экране (нужна только `\kf`).
    leftmost: LeftmostTracker,
}

/// Участок единого стиля с битмапами его глифов.
struct CollectedRun {
    /// Караоке участка; `None` — у слова нет эффекта.
    karaoke: Option<RunKaraoke>,
    colors: RunColors,
    /// Фильтры участка: флаги слоёв, размытие, смещение тени.
    filter: RunFilter,
    /// Дробная ошибка позиции первого глифа (общая для всех глифов участка).
    offset: [f64; 2],
    parts: Vec<GlyphBitmaps>,
    /// Сколько глифов участка осталось без обводки из-за сбоя её построения.
    skipped_borders: u32,
    /// Сколько глифов пропущено из-за предела стороны битмапа.
    skipped_oversized_bitmaps: u32,
}

/// Картинки реплики и отчёт о том, что пропущено при их построении.
#[derive(Debug)]
pub(crate) struct ComposedText {
    /// Картинки в порядке отрисовки.
    pub(crate) images: Vec<SubtitleBitmap>,
    /// Стадия караоке: правила участков и роли картинок; `None` — караоке нет.
    pub(crate) karaoke: Option<KaraokeStage>,
    /// Сколько глифов нарисовано без обводки из-за сбоя её построения.
    pub(crate) skipped_borders: u32,
    /// Сколько глифов пропущено: битмап после преобразования шире предела стороны.
    pub(crate) skipped_oversized_bitmaps: u32,
    /// Сколько слоёв осталось неразмытыми из-за нарушенного внутреннего инварианта размытия.
    pub(crate) failed_blur_layers: u32,
}

/// Строит картинки реплики. Позиции глифов в `clusters` переводятся из координат блока в
/// координаты кадра.
///
/// `blur_scratch` — рабочая память размытия, принадлежащая растеризатору (одна на поток
/// отрисовки); композиция только заимствует её и ничего в ней не хранит между вызовами.
/// `caches` - кэши глифов растеризатора: маски глифов (`collect_runs`) и склейки участков
/// (`composite_run`).
pub(crate) fn compose_text(
    clusters: &mut [TextCluster],
    input: &ComposeInput<'_>,
    blur_scratch: &mut BlurScratch,
    caches: &mut GlyphCaches,
) -> Result<ComposedText, ComposeError> {
    let runs = collect_runs(clusters, input, caches)?;
    let mut skipped_oversized_bitmaps = runs.iter().fold(0_u32, |total, run| {
        total.saturating_add(run.skipped_oversized_bitmaps)
    });
    let skipped_borders = runs.iter().fold(0_u32, |total, run| {
        total.saturating_add(run.skipped_borders)
    });
    let mut failed_blur_layers = 0_u32;
    let mut finished: Vec<FinishedRun> = Vec::with_capacity(runs.len());
    let mut karaoke_stage = KaraokeStageBuilder::default();
    let mut blur_env = BlurEnv {
        scratch: blur_scratch,
        side_limit: BitmapSideLimit(input.max_bitmap_side),
    };
    for run in &runs {
        if run.parts.is_empty() {
            continue;
        }
        let bitmaps = composite_run(
            &run.parts,
            &run.filter,
            &mut blur_env,
            &mut caches.composites,
        )?;
        // Слой, который не удалось размыть из-за размера, учитывается как пропущенный битмап.
        skipped_oversized_bitmaps =
            skipped_oversized_bitmaps.saturating_add(bitmaps.unblurred_oversized_layers);
        failed_blur_layers = failed_blur_layers.saturating_add(bitmaps.failed_blur_layers);
        let rule = run.karaoke.as_ref().map(|karaoke| {
            karaoke_stage.add_rule(KaraokeRunRule::new(RuleInputs {
                word: karaoke.word,
                first_origin_x: karaoke.first_origin_x,
                leftmost_x: karaoke.leftmost.min_x(),
                pixel_aspect_x: input.pixel_aspect_x,
                primary: run.colors.primary,
                secondary: run.colors.secondary,
            }))
        });
        finished.push(FinishedRun {
            colors: run.colors,
            bitmaps,
            rule,
        });
    }
    let images = render_text(&finished, input, &mut karaoke_stage)?;
    Ok(ComposedText {
        images,
        karaoke: karaoke_stage.finish(),
        skipped_borders,
        skipped_oversized_bitmaps,
        failed_blur_layers,
    })
}

/// Готовый участок: цвета, слои и правило караоке (индекс в стадии), если оно есть.
struct FinishedRun {
    colors: RunColors,
    bitmaps: RunBitmaps,
    rule: Option<usize>,
}

/// Раскладывает глифы по участкам и растеризует их (libass: `render_and_combine_glyphs`).
fn collect_runs(
    clusters: &mut [TextCluster],
    input: &ComposeInput<'_>,
    caches: &mut GlyphCaches,
) -> Result<Vec<CollectedRun>, ComposeError> {
    let mut rasterizer =
        Rasterizer::new(RASTERIZER_PRECISION).with_max_bitmap_side(input.max_bitmap_side);
    let rotation_center = input.layout.rotation_center(input.style);
    let mut runs: Vec<CollectedRun> = Vec::new();
    let mut new_run = true;
    // libass: `device_x = (device_x - left_margin) * par_scale_x + left_margin`, поля кадра нет.
    let device_x = input.layout.device_x * input.pixel_aspect_x;
    for (cluster_index, cluster) in clusters.iter_mut().enumerate() {
        if cluster.starts_new_run {
            new_run = true;
        }
        if cluster.skip {
            continue;
        }
        // Вид символа обязан быть в списке видов; иначе символ пропускается, а не паникует.
        let Some(look) = input.looks.get(cluster.look) else {
            continue;
        };
        let glyph_scale = cluster.glyph_scale;
        // Наклон считается по `\fscx`/`\fscy` рисунка, а не по масштабу его контура (libass:
        // у рисунка `info->scale_x` и `transform.scale` — разные величины).
        let shear_scale = if cluster.drawing.is_some() {
            look.scale
        } else {
            glyph_scale
        };
        let fade = cluster.fade;
        let letter_spacing_d6 = cluster.letter_spacing_d6;
        // Нулевой масштаб по любой оси — у буквы нет площади (libass пропускает такие глифы
        // при расчёте сдвига и ничего не получает от растеризатора).
        let has_area = glyph_scale.horizontal > 0.0 && glyph_scale.vertical > 0.0;
        let draws_nothing = cluster.symbol == '\n' || cluster.symbol == '\0';
        for glyph in cluster.glyphs_mut() {
            if new_run {
                // Слово караоке читается по первому видимому символу участка (libass копирует
                // `effect_*` первого глифа участка в объединённую картинку).
                let word = input.layout.karaoke.word_at(cluster_index);
                runs.push(CollectedRun {
                    karaoke: word.map(|(word, first_origin_x)| RunKaraoke {
                        word,
                        first_origin_x,
                        leftmost: LeftmostTracker::default(),
                    }),
                    // Затухание участка — затухание его ПЕРВОГО символа (libass не режет
                    // участки по затуханию и копирует цвета и `fade` первого `info`).
                    colors: RunColors {
                        primary: fade.apply_to(look.colors.primary),
                        secondary: fade.apply_to(look.colors.secondary),
                        outline: fade.apply_to(look.colors.outline),
                        back: fade.apply_to(look.colors.back),
                    },
                    filter: RunFilter::for_look_in_karaoke(
                        look,
                        fade,
                        word.map(|(word, _)| word.effect),
                    ),
                    offset: [0.0, 0.0],
                    parts: Vec::new(),
                    skipped_borders: 0,
                    skipped_oversized_bitmaps: 0,
                });
                new_run = false;
            }
            // Положение относительно центра поворота считается по позиции в блоке, поэтому
            // раньше перевода в координаты кадра.
            let shift = rotation::glyph_shift(glyph.pos, input, rotation_center, look);
            // Позиция в координатах кадра.
            glyph.pos.x = double_to_d6(device_x + d6_to_double(glyph.pos.x) * input.pixel_aspect_x);
            glyph.pos.y = glyph
                .pos
                .y
                .saturating_add(double_to_d6(input.layout.device_y));

            let Some(run) = runs.last_mut() else { continue };
            let Some(outline) = glyph.outline.clone() else {
                continue;
            };
            if draws_nothing || !has_area {
                continue;
            }
            let first = run.parts.is_empty();
            let request = GlyphBitmapRequest {
                glyph,
                shift,
                outline: &outline,
                glyph_scale,
                shear_scale,
                letter_spacing_d6,
                pixel_aspect_x: input.pixel_aspect_x,
                look,
                flags: run.filter.flags,
            };
            // `\kf`: самая левая точка участка считается по полностью преобразованному контуру,
            // даже если битмап глифа потом не получится (libass: до `quantize_transform`).
            if let Some(karaoke) = run.karaoke.as_mut()
                && karaoke.word.effect == KaraokeEffect::Fill
            {
                karaoke
                    .leftmost
                    .include_outline(&outline.outline, &glyph_matrices(&request).scaled);
            }
            let bitmaps = build_glyph_bitmaps(
                &request,
                &mut RunPhase {
                    rasterizer: &mut rasterizer,
                    outlines: &mut caches.outlines,
                    bitmaps: &mut caches.bitmaps,
                    offset: &mut run.offset,
                    first,
                },
            )?;
            if bitmaps.oversized {
                run.skipped_oversized_bitmaps = run.skipped_oversized_bitmaps.saturating_add(1);
            }
            if bitmaps.border_failed {
                run.skipped_borders = run.skipped_borders.saturating_add(1);
            }
            if bitmaps.fill.is_some() || bitmaps.outline.is_some() {
                run.parts.push(bitmaps);
            }
        }
    }
    Ok(runs)
}

/// Выдаёт слои в порядке libass: подложка, тени, обводки, заливки (libass: `render_text`).
///
/// `karaoke` получает роль каждой выданной картинки (по одной на картинку, в порядке выдачи).
fn render_text(
    finished: &[FinishedRun],
    input: &ComposeInput<'_>,
    karaoke: &mut KaraokeStageBuilder,
) -> Result<Vec<SubtitleBitmap>, ComposeError> {
    let mut images = Vec::new();
    if input.style.border_style == BorderStyle::EventBackground
        && let Some(background) = images::event_background(
            input.style,
            input.layout,
            input.canvas,
            input.pixel_aspect_x,
        )?
    {
        images.push(background);
        karaoke.push_image(ImageRole::Static);
    }
    let layers = [
        Layer {
            kind: SubtitleBitmapKind::Shadow,
            bitmap_of: |bitmaps| bitmaps.shadow.as_ref(),
            color_of: |colors| colors.back,
            role_of: |_| ImageRole::Static,
        },
        Layer {
            kind: SubtitleBitmapKind::Outline,
            bitmap_of: |bitmaps| bitmaps.outline.as_ref(),
            color_of: |colors| colors.outline,
            role_of: |rule| rule.map_or(ImageRole::Static, |rule| ImageRole::Outline { rule }),
        },
        Layer {
            kind: SubtitleBitmapKind::Glyph,
            bitmap_of: |bitmaps| bitmaps.fill.as_ref(),
            color_of: |colors| colors.primary,
            role_of: |rule| rule.map_or(ImageRole::Static, |rule| ImageRole::Fill { rule }),
        },
    ];
    for Layer {
        kind,
        bitmap_of,
        color_of,
        role_of,
    } in layers
    {
        if kind == SubtitleBitmapKind::Shadow
            && input.style.border_style == BorderStyle::EventBackground
        {
            // libass: при `BorderStyle = 4` тени не рисуются (их роль у подложки).
            continue;
        }
        for run in finished {
            let Some(bitmap) = bitmap_of(&run.bitmaps) else {
                continue;
            };
            let image = if kind == SubtitleBitmapKind::Glyph && run.rule.is_some() {
                // Цвет заливки караоке выбирается покадрово: прозрачный сейчас цвет не повод
                // выбросить картинку, пока прозрачны не оба цвета сразу.
                let invisible = run.colors.primary.alpha == 0 && run.colors.secondary.alpha == 0;
                if invisible {
                    None
                } else {
                    images::canvas_image(bitmap, color_of(&run.colors), kind, input.canvas)?
                }
            } else {
                images::clipped_image(bitmap, color_of(&run.colors), kind, input.canvas)?
            };
            if let Some(image) = image {
                images.push(image);
                karaoke.push_image(role_of(run.rule));
            }
        }
    }
    Ok(images)
}

#[cfg(test)]
mod tests;
