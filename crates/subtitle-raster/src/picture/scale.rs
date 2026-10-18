//! Масштабирование RGBA-картинки фильтром «среднее по площади» (box filter).
//!
//! Каждый пиксель результата - взвешенное среднее исходных пикселей, которые он накрывает;
//! вес - площадь перекрытия. Усреднение идёт в премультиплицированном пространстве (так
//! правильно смешиваются полупрозрачные края: цвет прозрачных пикселей не «протекает»).
//! Вся арифметика целочисленная, без плавающей точки: результат детерминирован.
//!
//! При уменьшении это качественное усреднение (при вдвое меньшем размере - среднее 2x2).
//! При увеличении фильтр даёт ближайший сосед для целых кратностей и смешение только на
//! границах исходных пикселей при дробных.

use super::geometry::VisibleWindow;

/// Вклад исходных пикселей одной оси в один пиксель результата.
struct AxisSpan {
    /// Индекс первого исходного пикселя.
    first: usize,
    /// Веса подряд идущих исходных пикселей; сумма весов равна длине исходной оси.
    weights: Vec<u64>,
}

/// Спаны для пикселей результата `destination_start..destination_start + count` оси, у
/// которой `source_len` исходных и `destination_len` итоговых пикселей.
///
/// Единицы: пиксель результата `d` занимает на исходной оси отрезок
/// `[d * source_len, (d + 1) * source_len)`, а исходный пиксель `i` - отрезок
/// `[i * destination_len, (i + 1) * destination_len)` (общий знаменатель - `destination_len`).
fn axis_spans(
    source_len: u32,
    destination_len: u32,
    destination_start: u32,
    count: u32,
) -> Vec<AxisSpan> {
    let source_len = u64::from(source_len);
    let destination_len = u64::from(destination_len);
    (destination_start..destination_start + count)
        .map(|index| {
            let from = u64::from(index) * source_len;
            let to = from + source_len;
            let first = from / destination_len;
            let last = (to - 1) / destination_len;
            let weights = (first..=last)
                .map(|pixel| {
                    let pixel_from = pixel * destination_len;
                    let pixel_to = pixel_from + destination_len;
                    to.min(pixel_to) - from.max(pixel_from)
                })
                .collect();
            AxisSpan {
                first: first as usize,
                weights,
            }
        })
        .collect()
}

/// Размеры исходной и итоговой картинки целиком (до обрезки по холсту).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScaleSizes {
    pub(crate) source_width: u32,
    pub(crate) source_height: u32,
    pub(crate) destination_width: u32,
    pub(crate) destination_height: u32,
}

/// Масштабирует `source` (RGBA, тесные строки) до `sizes.destination_*` и возвращает только
/// окно `window` итоговой картинки (остальное считать незачем - оно за холстом).
///
/// Результат - тесный RGBA `window.width x window.height`. `None`, если `source` короче,
/// чем требуют размеры (нарушен контракт декодера).
pub(crate) fn box_scale_window(
    source: &[u8],
    sizes: ScaleSizes,
    window: VisibleWindow,
) -> Option<Vec<u8>> {
    let source_width = sizes.source_width as usize;
    let source_height = sizes.source_height as usize;
    if source.len() < source_width.checked_mul(source_height)?.checked_mul(4)? {
        return None;
    }
    let window_width = window.width as usize;
    let window_height = window.height as usize;
    let mut result = Vec::with_capacity(window_width.checked_mul(window_height)?.checked_mul(4)?);

    let identity = sizes.source_width == sizes.destination_width
        && sizes.source_height == sizes.destination_height;
    if identity {
        // Масштаб 1: усреднять нечего, копируем строки окна.
        for row in 0..window_height {
            let start =
                ((window.skip_y as usize + row) * source_width + window.skip_x as usize) * 4;
            result.extend_from_slice(source.get(start..start + window_width * 4)?);
        }
        return Some(result);
    }

    let columns = axis_spans(
        sizes.source_width,
        sizes.destination_width,
        window.skip_x,
        window.width,
    );
    let rows = axis_spans(
        sizes.source_height,
        sizes.destination_height,
        window.skip_y,
        window.height,
    );
    // Знаменатель: сумма весов по x (= source_width) на сумму весов по y (= source_height).
    let denominator = u64::from(sizes.source_width) * u64::from(sizes.source_height);
    for row_span in &rows {
        for column_span in &columns {
            let mut sums = [0_u64; 4];
            for (row_offset, &row_weight) in row_span.weights.iter().enumerate() {
                let source_row = row_span.first + row_offset;
                for (column_offset, &column_weight) in column_span.weights.iter().enumerate() {
                    let offset =
                        (source_row * source_width + column_span.first + column_offset) * 4;
                    let texel = source.get(offset..offset + 4)?;
                    let weight = row_weight * column_weight;
                    for (sum, &channel) in sums.iter_mut().zip(texel) {
                        *sum += u64::from(channel) * weight;
                    }
                }
            }
            for sum in sums {
                // Среднее не превышает 255 (веса в сумме дают знаменатель).
                let channel = (sum + denominator / 2) / denominator;
                result.push(u8::try_from(channel).unwrap_or(u8::MAX));
            }
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(width: u32, height: u32) -> VisibleWindow {
        VisibleWindow {
            skip_x: 0,
            skip_y: 0,
            width,
            height,
            canvas_x: 0,
            canvas_y: 0,
        }
    }

    fn sizes(source: (u32, u32), destination: (u32, u32)) -> ScaleSizes {
        ScaleSizes {
            source_width: source.0,
            source_height: source.1,
            destination_width: destination.0,
            destination_height: destination.1,
        }
    }

    #[test]
    fn halving_averages_two_by_two_blocks_in_premultiplied_space() {
        // 2x2: красный непрозрачный, три прозрачных -> среднее = (255/4, 0, 0, 255/4).
        let source = [255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let result = box_scale_window(&source, sizes((2, 2), (1, 1)), whole(1, 1));
        assert_eq!(result, Some(vec![64, 0, 0, 64]));
    }

    #[test]
    fn integer_upscale_replicates_pixels() {
        let source = [10, 20, 30, 255, 40, 50, 60, 255];
        let result = box_scale_window(&source, sizes((2, 1), (4, 2)), whole(4, 2));
        let expected: Vec<u8> = [
            [10, 20, 30, 255],
            [10, 20, 30, 255],
            [40, 50, 60, 255],
            [40, 50, 60, 255],
        ]
        .iter()
        .cycle()
        .take(8)
        .flatten()
        .copied()
        .collect();
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn fractional_downscale_weights_by_overlap() {
        // 3 -> 2 по ширине: первый пиксель = (p0 * 2 + p1 * 1) / 3.
        let source = [0, 0, 0, 255, 90, 90, 90, 255, 180, 180, 180, 255];
        let result = box_scale_window(&source, sizes((3, 1), (2, 1)), whole(2, 1));
        // Пиксель 0: (0*2 + 90*1)/3 = 30; пиксель 1: (90*1 + 180*2)/3 = 150.
        assert_eq!(result, Some(vec![30, 30, 30, 255, 150, 150, 150, 255]));
    }

    #[test]
    fn window_returns_only_the_requested_part() {
        let source = [1, 1, 1, 255, 2, 2, 2, 255, 3, 3, 3, 255];
        let window = VisibleWindow {
            skip_x: 1,
            skip_y: 0,
            width: 2,
            height: 1,
            canvas_x: 0,
            canvas_y: 0,
        };
        let identity = box_scale_window(&source, sizes((3, 1), (3, 1)), window);
        assert_eq!(identity, Some(vec![2, 2, 2, 255, 3, 3, 3, 255]));
        let doubled = [1, 1, 1, 255, 3, 3, 3, 255];
        let upscaled = box_scale_window(
            &doubled,
            sizes((2, 1), (4, 1)),
            VisibleWindow {
                skip_x: 2,
                ..window
            },
        );
        assert_eq!(upscaled, Some(vec![3, 3, 3, 255, 3, 3, 3, 255]));
    }

    #[test]
    fn short_source_buffer_is_rejected() {
        assert_eq!(
            box_scale_window(&[0; 4], sizes((2, 2), (1, 1)), whole(1, 1)),
            None
        );
    }
}
