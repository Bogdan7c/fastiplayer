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
//! выдаются списком «все тени, все обводки, все заливки». Для `BorderStyle = 4` в начало
//! списка добавляется подложка, а тени не выдаются.

mod glyph_bitmaps;
mod images;
mod run;

use subtitle_core::SubtitleColor;

use crate::bitmap::BitmapError;
use crate::event_style::{BorderStyle, EventStyle};
use crate::fixed_point::{d6_to_double, double_to_d6};
use crate::glyph_info::TextCluster;
use crate::glyph_transform::RASTERIZER_PRECISION;
use crate::layout::TextLayout;
use crate::overlay::{CanvasSize, SubtitleBitmap, SubtitleBitmapError, SubtitleBitmapKind};
use crate::raster::Rasterizer;
use crate::source_text::SpanLook;
use glyph_bitmaps::{GlyphBitmapRequest, GlyphBitmaps, RunPhase, build_glyph_bitmaps};
use run::{FilterFlags, RunBitmaps, composite_run, quantized_shadow_offset};

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
    /// Внешние виды символов (цвет заливки).
    pub(crate) looks: &'a [SpanLook],
    /// Стиль реплики.
    pub(crate) style: &'a EventStyle,
    /// Результат раскладки.
    pub(crate) layout: &'a TextLayout,
    /// Холст.
    pub(crate) canvas: CanvasSize,
    /// Множитель `font_size / 256` (libass: `info->scale_x`).
    pub(crate) font_scale: f64,
}

/// Четыре цвета участка (libass: `c[0..4]`).
#[derive(Debug, Clone, Copy)]
struct RunColors {
    primary: SubtitleColor,
    outline: SubtitleColor,
    back: SubtitleColor,
}

/// Слой вывода: какой вид картинки, откуда взять битмап участка и каким цветом его красить.
struct Layer {
    kind: SubtitleBitmapKind,
    bitmap_of: fn(&RunBitmaps) -> Option<&crate::bitmap::CoverageBitmap>,
    color_of: fn(&RunColors) -> SubtitleColor,
}

/// Участок единого стиля с битмапами его глифов.
struct CollectedRun {
    colors: RunColors,
    flags: FilterFlags,
    /// Дробная ошибка позиции первого глифа (общая для всех глифов участка).
    offset: [f64; 2],
    parts: Vec<GlyphBitmaps>,
    /// Сколько глифов участка осталось без обводки из-за сбоя её построения.
    skipped_borders: u32,
}

/// Картинки реплики и отчёт о том, что пропущено при их построении.
#[derive(Debug)]
pub(crate) struct ComposedText {
    /// Картинки в порядке отрисовки.
    pub(crate) images: Vec<SubtitleBitmap>,
    /// Сколько глифов нарисовано без обводки из-за сбоя её построения.
    pub(crate) skipped_borders: u32,
}

/// Строит картинки реплики. Позиции глифов в `clusters` переводятся из координат блока в
/// координаты кадра.
pub(crate) fn compose_text(
    clusters: &mut [TextCluster],
    input: &ComposeInput<'_>,
) -> Result<ComposedText, ComposeError> {
    let runs = collect_runs(clusters, input)?;
    let skipped_borders = runs.iter().fold(0_u32, |total, run| {
        total.saturating_add(run.skipped_borders)
    });
    let mut finished: Vec<(RunColors, RunBitmaps)> = Vec::with_capacity(runs.len());
    let shadow_offset = quantized_shadow_offset(input.style);
    for run in &runs {
        if run.parts.is_empty() {
            continue;
        }
        finished.push((
            run.colors,
            composite_run(&run.parts, run.flags, shadow_offset)?,
        ));
    }
    Ok(ComposedText {
        images: render_text(&finished, input)?,
        skipped_borders,
    })
}

/// Раскладывает глифы по участкам и растеризует их (libass: `render_and_combine_glyphs`).
fn collect_runs(
    clusters: &mut [TextCluster],
    input: &ComposeInput<'_>,
) -> Result<Vec<CollectedRun>, ComposeError> {
    let style = input.style;
    let mut rasterizer = Rasterizer::new(RASTERIZER_PRECISION);
    let mut runs: Vec<CollectedRun> = Vec::new();
    let mut new_run = true;
    for cluster in clusters.iter_mut() {
        if cluster.starts_new_run {
            new_run = true;
        }
        if cluster.skip {
            continue;
        }
        let primary = input
            .looks
            .get(cluster.look)
            .and_then(|look| look.fill_override)
            .unwrap_or(style.colors.primary);
        let draws_nothing = cluster.symbol == '\n' || cluster.symbol == '\0';
        for glyph in cluster.glyphs_mut() {
            if new_run {
                runs.push(CollectedRun {
                    colors: RunColors {
                        primary,
                        outline: style.colors.outline,
                        back: style.colors.back,
                    },
                    flags: FilterFlags::for_glyph(style, primary),
                    offset: [0.0, 0.0],
                    parts: Vec::new(),
                    skipped_borders: 0,
                });
                new_run = false;
            }
            // Позиция в координатах кадра (par_scale_x = 1).
            glyph.pos.x = double_to_d6(input.layout.device_x + d6_to_double(glyph.pos.x) * 1.0);
            glyph.pos.y = glyph
                .pos
                .y
                .saturating_add(double_to_d6(input.layout.device_y));

            let Some(run) = runs.last_mut() else { continue };
            let Some(outline) = glyph.outline.clone() else {
                continue;
            };
            if draws_nothing {
                continue;
            }
            let first = run.parts.is_empty();
            let bitmaps = build_glyph_bitmaps(
                &GlyphBitmapRequest {
                    glyph,
                    outline: &outline,
                    font_scale: input.font_scale,
                    style,
                    flags: run.flags,
                },
                &mut RunPhase {
                    rasterizer: &mut rasterizer,
                    offset: &mut run.offset,
                    first,
                },
            )?;
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
fn render_text(
    finished: &[(RunColors, RunBitmaps)],
    input: &ComposeInput<'_>,
) -> Result<Vec<SubtitleBitmap>, ComposeError> {
    let mut images = Vec::new();
    if input.style.border_style == BorderStyle::EventBackground
        && let Some(background) = images::event_background(input.style, input.layout, input.canvas)?
    {
        images.push(background);
    }
    let layers = [
        Layer {
            kind: SubtitleBitmapKind::Shadow,
            bitmap_of: |bitmaps| bitmaps.shadow.as_ref(),
            color_of: |colors| colors.back,
        },
        Layer {
            kind: SubtitleBitmapKind::Outline,
            bitmap_of: |bitmaps| bitmaps.outline.as_ref(),
            color_of: |colors| colors.outline,
        },
        Layer {
            kind: SubtitleBitmapKind::Glyph,
            bitmap_of: |bitmaps| bitmaps.fill.as_ref(),
            color_of: |colors| colors.primary,
        },
    ];
    for Layer {
        kind,
        bitmap_of,
        color_of,
    } in layers
    {
        if kind == SubtitleBitmapKind::Shadow
            && input.style.border_style == BorderStyle::EventBackground
        {
            // libass: при `BorderStyle = 4` тени не рисуются (их роль у подложки).
            continue;
        }
        for (colors, bitmaps) in finished {
            let Some(bitmap) = bitmap_of(bitmaps) else {
                continue;
            };
            if let Some(image) =
                images::clipped_image(bitmap, color_of(colors), kind, input.canvas)?
            {
                images.push(image);
            }
        }
    }
    Ok(images)
}

#[cfg(test)]
mod tests;
