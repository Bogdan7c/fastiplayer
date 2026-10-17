//! Рисование одной реплики ASS: план -> текстовый конвейер -> плашка -> готовые картинки.
//!
//! Тонкая «склейка»: логика ASS (стиль, теги, геометрия) — в модуле `crate::ass`, рисование
//! букв — в общем текстовом конвейере (`render_source_text`). Здесь только то, что нужно
//! рендереру: картинки, рамки для столкновений и счётчики пропущенного.

use subtitle_core::AssCue;

use super::cue_render::{AssCollisionRect, CueRenderError, PixelRect, RasterizedCue, union_extent};
use crate::appearance::SubtitleAppearance;
use crate::ass::{AssRenderOptions, plan_ass_event};
use crate::face_cache::FaceCache;
use crate::fonts::{SubtitleFontLibrary, TextLanguagePreference};
use crate::layout::LineBox;
use crate::overlay::CanvasSize;
use crate::plate::build_plate;
use crate::render_diagnostics::RenderDiagnostics;
use crate::text_renderer::render_source_text;

/// Рисует реплику ASS на холсте.
///
/// `appearance` используется только при `AssStyleSource::User` (оформление пользователя и
/// плашка); при стиле автора он не влияет на результат.
pub(crate) fn rasterize_ass_cue(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    cue: &AssCue,
    appearance: &SubtitleAppearance,
    options: &AssRenderOptions,
    canvas: CanvasSize,
    language: TextLanguagePreference,
) -> Result<RasterizedCue, CueRenderError> {
    let plan = plan_ass_event(cue, canvas, options, appearance);
    if plan.skipped_drawing_runs > 0 {
        // Векторные рисунки (`\p`) приходят в слоях 2-3; текст реплики в журнал не пишем.
        tracing::debug!(
            runs = plan.skipped_drawing_runs,
            "ASS drawing mode is not supported, drawing commands are skipped"
        );
    }
    let rendered = render_source_text(fonts, faces, &plan.source, &plan.style, canvas, language)?;

    let mut bitmaps = Vec::with_capacity(rendered.images.len() + 1);
    // Плашка пользовательского стиля лежит под текстом (как у простых форматов).
    if let Some(plate_style) = plan.plate {
        // Текст растягивается по горизонтали на `par_scale_x` при композиции, плашка — здесь.
        let pixel_aspect_x = plan.style.pixel_aspect_x();
        let lines: Vec<LineBox> = rendered
            .lines
            .iter()
            .map(|line| LineBox {
                left: line.left * pixel_aspect_x,
                right: line.right * pixel_aspect_x,
                ..*line
            })
            .collect();
        if let Some(plate) = build_plate(&lines, &plate_style, canvas)? {
            bitmaps.push(plate);
        }
    }
    bitmaps.extend(rendered.images);

    // Рамка столкновений — по метрикам текста; пустая рамка (ширина или высота 0) в
    // столкновениях не участвует, как в libass.
    let ass_collision = rendered.event_rect.map(|rect| AssCollisionRect {
        rect: PixelRect {
            left: rect.left,
            top: rect.top,
            right: rect.left.saturating_add(rect.width),
            bottom: rect.top.saturating_add(rect.height),
        },
        detect_collisions: plan.collision.detect && rect.width > 0 && rect.height > 0,
    });
    // Группа размещения — свойство реплики: помечаем все её картинки, включая плашку.
    let bitmaps = bitmaps
        .into_iter()
        .map(|bitmap| bitmap.with_placement_group(plan.placement_group))
        .collect::<Vec<_>>();
    Ok(RasterizedCue {
        extent: union_extent(&bitmaps),
        bitmaps,
        collision: plan.collision.direction,
        ass_collision,
        diagnostics: rendered.diagnostics.combined(RenderDiagnostics {
            skipped_ass_drawings: plan.skipped_drawing_runs,
            ..RenderDiagnostics::default()
        }),
    })
}
