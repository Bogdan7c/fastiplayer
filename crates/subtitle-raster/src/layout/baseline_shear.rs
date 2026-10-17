// Порт из libass 0.17.5: ass_render.c (apply_baseline_shear).
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

//! Сдвиг базовой линии для `\fay`.
//!
//! Наклон `\fay` в матрице глифа наклоняет только сам контур вокруг его начала. Чтобы
//! соседние буквы выстроились вдоль наклонной линии, libass дополнительно поднимает (опускает)
//! положение каждого следующего глифа на накопленное смещение: `shear += fay * advance.x`.
//! Накопление обнуляется на каждом переносе строки и на границе участка единого стиля.
//!
//! Это шаг раскладки (меняет `pos.y`), поэтому живёт рядом с выравниванием и НЕ влияет на
//! габариты блока: рамка текста считается до него, как в libass.

use crate::glyph_info::TextCluster;

/// Применяет сдвиг базовой линии. `visual_order` — карта «визуальная позиция -> логический
/// индекс» из [`super::reorder::reorder_text`].
///
/// Флаги границ (`перенос`, `новый участок`) читаются по ВИЗУАЛЬНОМУ индексу, а глиф — по
/// логическому: так делает и libass (`glyphs[i].linebreak` рядом с `glyphs + cmap[i]`).
pub(super) fn apply_baseline_shear(clusters: &mut [TextCluster], visual_order: &[usize]) {
    // Без `\fay` ни один глиф не сдвигается: обход не нужен.
    if clusters.iter().all(|cluster| cluster.baseline_shear == 0.0) {
        return;
    }
    let mut accumulated_shear: i32 = 0;
    for (visual, logical) in visual_order.iter().copied().enumerate() {
        let resets_shear = clusters
            .get(visual)
            .is_some_and(|cluster| cluster.line_start.begins_line() || cluster.starts_new_run);
        if resets_shear {
            accumulated_shear = 0;
        }
        let Some(cluster) = clusters.get_mut(logical) else {
            continue;
        };
        let (scale_x, scale_y) = (cluster.glyph_scale.horizontal, cluster.glyph_scale.vertical);
        // Буква без площади не рисуется и сдвиг не накапливает.
        if cluster.skip || scale_x == 0.0 || scale_y == 0.0 {
            continue;
        }
        let fay = cluster.baseline_shear / scale_x * scale_y;
        for glyph in cluster.glyphs_mut() {
            // Целочисленные присваивания libass усекают дробную часть к нулю.
            let lift = f64::from(accumulated_shear) + fay * f64::from(glyph.offset.x);
            glyph.pos.y = (f64::from(glyph.pos.y) + lift) as i32;
            accumulated_shear =
                (f64::from(accumulated_shear) + fay * f64::from(glyph.advance.x)) as i32;
        }
    }
}
