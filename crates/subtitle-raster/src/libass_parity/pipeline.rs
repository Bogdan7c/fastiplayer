//! Воспроизведение пути libass 0.17.5 от рисунка `\p1` до списка картинок.
//!
//! Повторяются только «клеевые» шаги `ass_render.c` для случая «один глиф-рисунок, без
//! поворотов и перспективы» (`calc_transform_matrix`, `quantize_transform`,
//! `restore_transform`, `get_bitmap_glyph`, `render_and_combine_glyphs`,
//! `ass_composite_construct`, `render_text`); растеризация, обводка, `fix_outline` и
//! `shift_bitmap` выполняются кодом порта. Числа и порядок операций с `double` сохранены,
//! чтобы квантование позиции (1/8 пикселя) и масштаба совпадало с libass.

use subtitle_core::SubtitleColor;

use super::ass_case::DrawingCase;
use crate::bitmap::{CoverageBitmap, fix_outline, shadow_bitmap};
use crate::glyph_transform::{
    Matrix3, RASTERIZER_PRECISION, STROKER_PRECISION, border_matrix, construct_bitmap, control_box,
    plan_border_stroke, quantize_transform, scale_and_offset_matrix, unrotated_glyph_matrix,
};
use crate::outline::stroke::{StrokeParameters, stroke_outline};
use crate::outline::{Outline, Point, round_ties_even_to_i32};
use crate::overlay::SubtitleBitmapKind;
use crate::raster::Rasterizer;

/// Итоговая картинка, как `ASS_Image`.
#[derive(Debug, Clone)]
pub(super) struct RenderedImage {
    pub(super) kind: SubtitleBitmapKind,
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) stride: usize,
    pub(super) color: SubtitleColor,
    pub(super) mask: Vec<u8>,
}

/// Рендерит кейс и возвращает картинки в порядке libass: тень, обводка, заливка.
pub(super) fn render_case(case: &DrawingCase) -> Vec<RenderedImage> {
    let mut rasterizer = Rasterizer::new(RASTERIZER_PRECISION);
    let empty = Outline::new();
    let drawing_box = control_box(&[&case.drawing]);

    // --- Позиция глифа (libass: ass_render_event + render_and_combine_glyphs) ---
    let frame_width = f64::from(case.width);
    let frame_height = f64::from(case.height);
    let asc = drawing_box.y_max - drawing_box.y_min;
    let line_ascent = (1.0 / 64.0) * f64::from(asc);
    // x2scr_pos / y2scr_pos при PlayRes = размеру кадра, par = 1, без полей.
    let device_x = case.position.0 * frame_width / 1.0 / frame_width;
    let device_y = case.position.1 * frame_height / frame_height - (-line_ascent);
    let pos_x = round_ties_even_to_i32(device_x * 64.0);
    let pos_y = round_ties_even_to_i32(device_y * 64.0);

    // --- Матрица глифа без поворотов (calc_transform_matrix + get_bitmap_glyph) ---
    // Рисунок: масштаб 1, сдвиг по Y на -asc (libass: `offset.y = -asc * scale.y`).
    let glyph_offset_y = -f64::from(asc);
    let m2 = scale_and_offset_matrix(
        &unrotated_glyph_matrix(pos_x, pos_y),
        (1.0, 1.0),
        (0.0, glyph_offset_y),
    );
    let mut matrix: Matrix3 = m2;

    let mut offset = [0.0_f64; 2];
    let Some(fill_key) = quantize_transform(&mut matrix, &mut offset, true, &drawing_box) else {
        return Vec::new();
    };
    let fill = construct_bitmap(
        &mut rasterizer,
        &fill_key,
        &drawing_box,
        [&case.drawing, &empty],
    )
    .expect("fill rasterization must succeed");
    let fill_position = fill_key.position;

    // --- Обводка ---
    let mut outline_bitmap: Option<(CoverageBitmap, Point)> = None;
    let nonzero_border = case.border != 0.0;
    if nonzero_border {
        let border = (
            64.0 * 1.0 * case.border / 1.0 / 1.0,
            64.0 * 1.0 * case.border / 1.0,
        );
        let plan = plan_border_stroke(&matrix, &drawing_box, border).expect("border must fit");
        assert!(
            plan.border_x != 0 || plan.border_y != 0,
            "case border must survive rounding"
        );

        let source = case
            .drawing
            .scaled_pow2(plan.scale_order_x, plan.scale_order_y)
            .expect("scale must fit");
        let stroked = stroke_outline(
            &source,
            StrokeParameters {
                border_x: plan.border_x * STROKER_PRECISION,
                border_y: plan.border_y * STROKER_PRECISION,
                precision: STROKER_PRECISION,
            },
        )
        .expect("stroke must succeed");

        let mut stroked_matrix = border_matrix(&m2, &plan);
        let border_box = control_box(&[&stroked.positive, &stroked.negative]);
        if let Some(border_key) =
            quantize_transform(&mut stroked_matrix, &mut offset, false, &border_box)
        {
            let bitmap = construct_bitmap(
                &mut rasterizer,
                &border_key,
                &border_box,
                [&stroked.positive, &stroked.negative],
            )
            .expect("border rasterization must succeed");
            outline_bitmap = bitmap.map(|bitmap| (bitmap, border_key.position));
        }
    }

    // --- Фильтры и тень (render_and_combine_glyphs / ass_composite_construct) ---
    let [primary, secondary, outline_color, back] = case.colors;
    let nonzero_shadow = case.shadow != 0.0;
    let fill_in_shadow = nonzero_shadow && primary.transparency != 0xFF;
    let keep_shadow = nonzero_shadow && (nonzero_border || fill_in_shadow);
    let fill_in_border = nonzero_border && primary.transparency == 0 && secondary.transparency == 0;

    let fill = fill.map(|mut bitmap| {
        bitmap.left += fill_position.x;
        bitmap.top += fill_position.y;
        bitmap
    });
    let mut outline_bitmap = outline_bitmap.map(|(mut bitmap, position)| {
        bitmap.left += position.x;
        bitmap.top += position.y;
        bitmap
    });

    if !fill_in_border
        && !fill_in_shadow
        && let (Some(glyph), Some(outline)) = (fill.as_ref(), outline_bitmap.as_mut())
    {
        fix_outline(glyph, outline).expect("fix_outline must succeed");
    }
    let mut shadow: Option<CoverageBitmap> = None;
    if keep_shadow {
        // Квантование смещения тени: маска 7 при blur = 0 (quantize_blur).
        let shadow_mask = 7;
        let raw = round_ties_even_to_i32(case.shadow * 64.0);
        let shadow_offset = (raw + (shadow_mask >> 1)) & !shadow_mask;
        let source = if nonzero_border {
            outline_bitmap.clone()
        } else {
            fill.clone()
        };
        if let Some(mut source) = source {
            if nonzero_border
                && fill_in_border
                && !fill_in_shadow
                && let Some(glyph) = fill.as_ref()
            {
                fix_outline(glyph, &mut source).expect("fix_outline must succeed");
            }
            shadow = Some(
                shadow_bitmap(&source, shadow_offset, shadow_offset).expect("shadow must succeed"),
            );
        }
    }
    if fill_in_shadow
        && !fill_in_border
        && let (Some(glyph), Some(outline)) = (fill.as_ref(), outline_bitmap.as_mut())
    {
        fix_outline(glyph, outline).expect("fix_outline must succeed");
    }

    // --- Порядок отрисовки: тень, обводка, заливка; невидимые слои libass пропускает ---
    let mut images = Vec::new();
    let layers = [
        (shadow, back, SubtitleBitmapKind::Shadow),
        (outline_bitmap, outline_color, SubtitleBitmapKind::Outline),
        (fill, primary, SubtitleBitmapKind::Glyph),
    ];
    for (bitmap, color, kind) in layers {
        let Some(bitmap) = bitmap else { continue };
        if color.transparency == 0xFF {
            continue;
        }
        images.push(RenderedImage {
            kind,
            x: bitmap.left,
            y: bitmap.top,
            width: bitmap.width,
            height: bitmap.height,
            stride: bitmap.stride,
            color: SubtitleColor {
                red: color.red,
                green: color.green,
                blue: color.blue,
                alpha: 255 - color.transparency,
            },
            mask: bitmap.buffer,
        });
    }
    images
}
