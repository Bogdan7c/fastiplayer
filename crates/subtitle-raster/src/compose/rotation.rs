// Порт из libass 0.17.5: ass_render.c (calculate_rotation_params).
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

//! Положение глифа относительно центра поворота.
//!
//! Матрица глифа ([`crate::glyph_transform::glyph_transform_matrix`]) вращает глиф вокруг
//! начала координат, поэтому сначала положение глифа переводится в систему «центр поворота =
//! начало»: `shift = позиция в блоке + (положение блока - центр)`. В `shift` входит и смещение
//! тени: libass вращает глиф вместе с тенью, а потом смещение тени вычитается из итоговой
//! позиции (см. `offs_x` в `calc_transform_matrix`), поэтому без поворота тень остаётся на
//! месте, а при повороте слегка «уезжает» вместе с углом, как у libass.

use super::ComposeInput;
use crate::fixed_point::{Vector26, double_to_d6};
use crate::layout::RotationCenter;
use crate::span_look::SpanLook;

/// Положение глифа относительно центра поворота, 26.6 (libass: `info->shift`).
///
/// `block_position` — позиция глифа в блоке текста (до перевода в координаты кадра);
/// `center` — центр поворота реплики.
pub(super) fn glyph_shift(
    block_position: Vector26,
    input: &ComposeInput<'_>,
    center: RotationCenter,
    look: &SpanLook,
) -> Vector26 {
    let layout = input.layout;
    // Тень пересчитывается в единицы до растяжения на `par_scale_x`, как и позиции блока.
    let shadow_x = look.shadow.horizontal / input.pixel_aspect_x;
    let shadow_y = look.shadow.vertical;
    Vector26 {
        x: block_position
            .x
            .saturating_add(double_to_d6(layout.device_x - center.x + shadow_x)),
        y: block_position
            .y
            .saturating_add(double_to_d6(layout.device_y - center.y + shadow_y)),
    }
}
