//! Рисование одной реплики ASS: план -> текстовый конвейер -> плашка -> готовые картинки.
//!
//! Тонкая «склейка»: логика ASS (стиль, теги, геометрия) — в модуле `crate::ass`, рисование
//! букв — в общем текстовом конвейере (`render_source_text`). Здесь только то, что нужно
//! рендереру: картинки, рамки для столкновений и счётчики пропущенного.

use std::sync::OnceLock;

use subtitle_core::AssCue;

use super::cue_render::{AssCollisionRect, CueRenderError, PixelRect, RasterizedCue, union_extent};
use crate::appearance::SubtitleAppearance;
use crate::ass::{AssAnimationState, AssRenderOptions, EventClipPlan, plan_ass_event};
use crate::blur::BlurScratch;
use crate::clip::{CueClip, IgnoredVectorClip};
use crate::face_cache::FaceCache;
use crate::fonts::{SubtitleFontLibrary, TextLanguagePreference};
use crate::layout::LineBox;
use crate::overlay::CanvasSize;
use crate::plate::build_plate;
use crate::render_diagnostics::RenderDiagnostics;
use crate::text_renderer::{BitmapWorkspace, render_source_text};

/// Входы рисования реплики ASS, общие для кадра.
pub(crate) struct AssCueInputs<'a> {
    /// Стиль пользователя: используется только при `AssStyleSource::User` (оформление
    /// пользователя и плашка); при стиле автора он не влияет на результат.
    pub(crate) appearance: &'a SubtitleAppearance,
    /// Настройки рисования ASS.
    pub(crate) options: &'a AssRenderOptions,
    /// Состояние анимации на момент показа (положение `\move`, затухание).
    pub(crate) animation: &'a AssAnimationState,
    /// Холст.
    pub(crate) canvas: CanvasSize,
    /// Язык текста для выбора региональной формы CJK.
    pub(crate) language: TextLanguagePreference,
    /// Наибольшая сторона битмапа глифа (`RasterInputLimits::max_canvas_side`).
    pub(crate) max_bitmap_side: u32,
}

/// Рисует реплику ASS на холсте.
pub(crate) fn rasterize_ass_cue(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    cue: &AssCue,
    inputs: &AssCueInputs<'_>,
    blur_scratch: &mut BlurScratch,
) -> Result<RasterizedCue, CueRenderError> {
    let AssCueInputs {
        appearance,
        options,
        animation,
        canvas,
        language,
        max_bitmap_side,
    } = *inputs;
    let plan = plan_ass_event(cue, canvas, options, appearance, animation);
    let rendered = render_source_text(
        fonts,
        faces,
        &plan.source,
        &plan.style,
        canvas,
        language,
        BitmapWorkspace {
            max_bitmap_side,
            blur_scratch,
        },
    )?;

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
    let mut diagnostics = rendered.diagnostics;
    let clip = build_clip(&plan.clip, canvas, &mut diagnostics)?;
    Ok(RasterizedCue {
        extent: union_extent(&bitmaps),
        bitmaps,
        collision: plan.collision.direction,
        ass_collision,
        diagnostics,
        clip,
        clipped_bitmaps: OnceLock::new(),
    })
}

/// Собирает клип реплики и записывает в отчёт, что из векторного клипа не применяется.
///
/// libass при отвергнутом рисунке клипа просто не клипует (`!outline->valid`); у нас это же,
/// но с записью в счётчики, чтобы пропуск был виден.
fn build_clip(
    plan: &EventClipPlan,
    canvas: CanvasSize,
    diagnostics: &mut RenderDiagnostics,
) -> Result<CueClip, CueRenderError> {
    let built = CueClip::build(plan.rect, plan.vector.as_ref(), canvas)?;
    match built.ignored_vector {
        None => {}
        Some(IgnoredVectorClip::InvalidDrawing | IgnoredVectorClip::Untransformable) => {
            diagnostics.invalid_ass_drawings = diagnostics.invalid_ass_drawings.saturating_add(1);
        }
    }
    Ok(built.clip)
}
