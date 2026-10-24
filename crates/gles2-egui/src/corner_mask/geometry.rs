//! Геометрия маски углов: 4 угловых квадрата вместо полноэкранного прохода.
//!
//! Формула покрытия — ровно `window_corner_mask.wgsl` у wgpu:
//! `q = |p − половина| − (половина − r)`,
//! `d = |max(q, 0)| + min(max(q.x, q.y), 0) − r`,
//! `покрытие = clamp(0,5 − d / 1 px, 0, 1)`.
//!
//! Почему хватает углов. Вне квадрата со стороной `ceil(r) + 1` от угла хотя бы одна
//! компонента `q` ≤ −0,5 px. Тогда `d ≤ −0,5`, покрытие = 1, и умножение на него оставляет
//! пиксель как был. Полноэкранный проход у wgpu там тоже ничего не меняет, поэтому пиксели
//! совпадают, а GPU (на Ironlake это заметно) не красит лишние ~1,3 млн пикселей.
//!
//! Квадраты не перекрываются, даже когда радиус равен половине меньшей стороны: левая колонка
//! кончается на `w/2`, правая начинается не раньше её конца. Иначе пиксель в перекрытии
//! умножился бы на покрытие дважды — и разошёлся бы с wgpu.
//!
//! Точность. В вершину кладётся не координата пикселя, а расстояние до ближайших краёв окна
//! (`q = r − расстояние`). Значения маленькие (≤ r + 1): координата 1440 в fp16 потеряла бы
//! половинки пикселя, а расстояние — нет. Сама формула всё равно считается в `highp`, где он
//! есть (см. `corner_mask::FRAGMENT_SHADER_BODY`).

/// Ширина сглаживания края, физические пиксели (как `antialias_width = 1.0` у wgpu).
pub(super) const ANTIALIAS_WIDTH_PIXELS: f32 = 1.0;

/// Вершина маски: позиция в NDC и расстояние до ближайших вертикального и горизонтального краёв
/// цели в пикселях.
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub(super) struct CornerVertex {
    pub(super) ndc: [f32; 2],
    pub(super) edge_distance: [f32; 2],
}

/// Вершин на маску: 4 квадрата × 2 треугольника × 3 вершины.
pub(super) const CORNER_MASK_VERTEX_COUNT: usize = 24;

/// Прямоугольник в пикселях цели (начало — верхний левый угол), полуоткрытый `[min, max)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PixelSpan {
    pub(super) min: u32,
    pub(super) max: u32,
}

/// Разбиение одной оси на «ближнюю» и «дальнюю» полосы углов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AxisCorners {
    pub(super) near: PixelSpan,
    pub(super) far: PixelSpan,
}

/// Радиус в физических пикселях: точки × масштаб, не больше половины меньшей стороны (как у
/// wgpu). `None` — квадратные углы (0, отрицательный, NaN, бесконечность, пустая цель).
pub(super) fn physical_radius(
    radius_points: f32,
    pixels_per_point: f32,
    target_size: [u32; 2],
) -> Option<f32> {
    let [width, height] = target_size.map(|side| side as f32);
    let radius = (radius_points * pixels_per_point).clamp(0.0, width.min(height) * 0.5);
    // `!(radius > 0)` ловит и NaN: `clamp` возвращает NaN, если NaN на входе.
    (radius > 0.0 && radius.is_finite()).then_some(radius)
}

/// Полосы углов по одной оси длины `extent`.
pub(super) fn axis_corners(extent: u32, radius: f32) -> AxisCorners {
    // Сторона квадрата: радиус + полоса сглаживания, с запасом до целого пикселя.
    let side = (radius + ANTIALIAS_WIDTH_PIXELS).ceil() as u32;
    let near_end = side.min(extent / 2);
    let far_start = extent.saturating_sub(side).max(near_end);
    AxisCorners {
        near: PixelSpan {
            min: 0,
            max: near_end,
        },
        far: PixelSpan {
            min: far_start,
            max: extent,
        },
    }
}

/// От какого края окна считается расстояние по оси.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeasuredFrom {
    /// От левого (верхнего) края: расстояние = координата.
    NearEdge,
    /// От правого (нижнего) края: расстояние = длина − координата.
    FarEdge,
}

/// Вершины 4 угловых квадратов (порядок: верх-лево, верх-право, низ-лево, низ-право).
pub(super) fn corner_vertices(
    target_size: [u32; 2],
    radius: f32,
) -> [CornerVertex; CORNER_MASK_VERTEX_COUNT] {
    let columns = axis_corners(target_size[0], radius);
    let rows = axis_corners(target_size[1], radius);
    let near = MeasuredFrom::NearEdge;
    let far = MeasuredFrom::FarEdge;
    let corners = [
        ((columns.near, near), (rows.near, near)),
        ((columns.far, far), (rows.near, near)),
        ((columns.near, near), (rows.far, far)),
        ((columns.far, far), (rows.far, far)),
    ];
    let mut vertices = [CornerVertex {
        ndc: [0.0; 2],
        edge_distance: [0.0; 2],
    }; CORNER_MASK_VERTEX_COUNT];
    for (corner_index, (column, row)) in corners.into_iter().enumerate() {
        let quad = quad_vertices(column, row, target_size);
        vertices[corner_index * 6..corner_index * 6 + 6].copy_from_slice(&quad);
    }
    vertices
}

/// Два треугольника прямоугольника `column × row`. Пустой прямоугольник даёт вырожденные
/// треугольники — GPU их не растеризует.
fn quad_vertices(
    column: (PixelSpan, MeasuredFrom),
    row: (PixelSpan, MeasuredFrom),
    target_size: [u32; 2],
) -> [CornerVertex; 6] {
    let vertex = |x: u32, y: u32| corner_vertex([x, y], [column.1, row.1], target_size);
    let top_left = vertex(column.0.min, row.0.min);
    let top_right = vertex(column.0.max, row.0.min);
    let bottom_left = vertex(column.0.min, row.0.max);
    let bottom_right = vertex(column.0.max, row.0.max);
    [
        top_left,
        top_right,
        bottom_left,
        bottom_left,
        top_right,
        bottom_right,
    ]
}

/// Вершина в точке пиксельной сетки (начало сверху слева).
///
/// Расстояние считается от края «своего» угла по всему квадрату — тогда оно линейно, и
/// интерполяция даёт в центре пикселя ровно `half − |p − half|` из формулы wgpu. (Брать
/// `min(x, w − x)` в вершинах нельзя: при нечётной стороне дальний квадрат может начаться
/// левее середины, и интерполяция между ветками исказит покрытие.)
fn corner_vertex(
    point: [u32; 2],
    measured_from: [MeasuredFrom; 2],
    target_size: [u32; 2],
) -> CornerVertex {
    let [width, height] = target_size.map(|side| side as f32);
    let [x, y] = point.map(|coordinate| coordinate as f32);
    let distance = |coordinate: f32, extent: f32, from: MeasuredFrom| match from {
        MeasuredFrom::NearEdge => coordinate,
        MeasuredFrom::FarEdge => extent - coordinate,
    };
    CornerVertex {
        ndc: [2.0 * x / width - 1.0, 1.0 - 2.0 * y / height],
        edge_distance: [
            distance(x, width, measured_from[0]),
            distance(y, height, measured_from[1]),
        ],
    }
}

/// Покрытие пикселя по формуле wgpu (эталон для тестов; `pixel` — центр пикселя).
#[cfg(test)]
pub(super) fn reference_coverage(pixel: [f32; 2], target_size: [u32; 2], radius: f32) -> f32 {
    let half = target_size.map(|side| side as f32 * 0.5);
    let q = [
        (pixel[0] - half[0]).abs() - (half[0] - radius),
        (pixel[1] - half[1]).abs() - (half[1] - radius),
    ];
    let outside = q[0].max(0.0).hypot(q[1].max(0.0));
    let distance = outside + q[0].max(q[1]).min(0.0) - radius;
    (0.5 - distance / ANTIALIAS_WIDTH_PIXELS).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::{axis_corners, physical_radius, reference_coverage};

    #[test]
    fn radius_follows_wgpu_rules() {
        assert_eq!(physical_radius(12.0, 1.5, [800, 600]), Some(18.0));
        assert_eq!(physical_radius(400.0, 1.0, [64, 40]), Some(20.0));
        assert_eq!(physical_radius(0.0, 1.0, [64, 40]), None);
        assert_eq!(physical_radius(f32::NAN, 1.0, [64, 40]), None);
        assert_eq!(physical_radius(12.0, f32::NAN, [64, 40]), None);
        assert_eq!(physical_radius(-3.0, 1.0, [64, 40]), None);
    }

    #[test]
    fn corner_spans_never_overlap_even_at_half_side_radius() {
        for extent in [1_u32, 2, 5, 40, 41, 64] {
            let radius = extent as f32 * 0.5;
            let corners = axis_corners(extent, radius);
            assert!(corners.near.max <= corners.far.min, "extent {extent}");
            assert_eq!(corners.far.max, extent);
        }
    }

    /// Внутри квадратов расстояние «от края своего угла» даёт то же покрытие, что формула wgpu
    /// от центра окна, — и для нечётных сторон, где дальний квадрат начинается у середины.
    #[test]
    fn corner_relative_distance_reproduces_wgpu_coverage_inside_squares() {
        for (size, radius) in [
            ([64_u32, 64_u32], 12.0_f32),
            ([5, 7], 2.5),
            ([97, 51], 25.5),
        ] {
            let columns = axis_corners(size[0], radius);
            let rows = axis_corners(size[1], radius);
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let distance_x = if x < columns.near.max {
                        px
                    } else if x >= columns.far.min {
                        size[0] as f32 - px
                    } else {
                        continue;
                    };
                    let distance_y = if y < rows.near.max {
                        py
                    } else if y >= rows.far.min {
                        size[1] as f32 - py
                    } else {
                        continue;
                    };
                    let q = [radius - distance_x, radius - distance_y];
                    let ours = (0.5
                        - (q[0].max(0.0).hypot(q[1].max(0.0)) + q[0].max(q[1]).min(0.0) - radius))
                        .clamp(0.0, 1.0);
                    let expected = reference_coverage([px, py], size, radius);
                    assert!(
                        (ours - expected).abs() < 1e-4,
                        "size {size:?} radius {radius} pixel ({x}, {y}): {ours} vs {expected}"
                    );
                }
            }
        }
    }

    /// Вне угловых квадратов покрытие ровно 1: маска там ничего не меняет.
    #[test]
    fn coverage_outside_corner_squares_is_exactly_one() {
        for (size, radius) in [
            ([64_u32, 64_u32], 12.0_f32),
            ([64, 40], 20.0),
            ([97, 51], 7.3),
        ] {
            let columns = axis_corners(size[0], radius);
            let rows = axis_corners(size[1], radius);
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let in_column = x < columns.near.max || x >= columns.far.min;
                    let in_row = y < rows.near.max || y >= rows.far.min;
                    if in_column && in_row {
                        continue;
                    }
                    let coverage =
                        reference_coverage([x as f32 + 0.5, y as f32 + 0.5], size, radius);
                    assert_eq!(
                        coverage, 1.0,
                        "size {size:?} radius {radius} pixel ({x}, {y})"
                    );
                }
            }
        }
    }
}
