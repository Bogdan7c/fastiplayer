use super::*;
use crate::blur::gauss_kernels::{
    Plane, RowScratch, blur_horz_dispatch, blur_vert_dispatch, expand_func, expand_horz,
    expand_vert, shrink_func, shrink_horz, shrink_vert,
};

// ---------------------------------------------------------------------------------------
// Ядра: сверка с прямой реализацией формул C на нулевом продолжении
// ---------------------------------------------------------------------------------------

/// Двумерный образ плоскости: `rows[y][x]`, за краем нули.
struct Grid(Vec<Vec<i16>>);

impl Grid {
    fn get(&self, x: isize, y: isize) -> i16 {
        usize::try_from(y)
            .ok()
            .and_then(|y| self.0.get(y))
            .and_then(|row| usize::try_from(x).ok().and_then(|x| row.get(x)))
            .copied()
            .unwrap_or(0)
    }

    fn width(&self) -> usize {
        self.0.first().map_or(0, Vec::len)
    }

    fn height(&self) -> usize {
        self.0.len()
    }

    fn transposed(&self) -> Grid {
        Grid(
            (0..self.width())
                .map(|x| self.0.iter().map(|row| row[x]).collect())
                .collect(),
        )
    }
}

/// Случайная плоскость со значениями `0..=0x4000` и нулями за `width`.
fn random_plane(width: usize, height: usize, seed: u64) -> (Plane, Vec<i16>, Grid) {
    let plane = Plane { width, height };
    let noise = noise_bytes(width * height * 2, seed);
    let mut data = vec![0i16; plane.len()];
    let mut grid = Vec::new();
    for y in 0..height {
        let mut row = Vec::new();
        for x in 0..width {
            let raw =
                u16::from_le_bytes([noise[2 * (y * width + x)], noise[2 * (y * width + x) + 1]]);
            let value = (raw % 0x4001) as i16;
            data[y * plane.stride() + x] = value;
            row.push(value);
        }
        grid.push(row);
    }
    (plane, data, Grid(grid))
}

/// Плоскость ядра как `Grid`; также проверяет, что правее `width` нули.
fn plane_to_grid(plane: Plane, data: &[i16]) -> Grid {
    let stride = plane.stride();
    let mut rows = Vec::new();
    for y in 0..plane.height {
        let row = &data[y * stride..(y + 1) * stride];
        assert!(
            row[plane.width..].iter().all(|&v| v == 0),
            "ненулевой хвост строки {y}"
        );
        rows.push(row[..plane.width].to_vec());
    }
    Grid(rows)
}

/// Прямая запись `blur_horz` из C: `dst[j] = c + ((0x8000 + Σ ...) >> 16)`.
fn naive_blur_horz(src: &Grid, radius: usize, weights: &[i16; 8]) -> Grid {
    let n = radius as isize;
    Grid(
        (0..src.height() as isize)
            .map(|y| {
                (0..src.width() as isize + 2 * n)
                    .map(|j| {
                        let center = src.get(j - n, y);
                        let mut acc = 0x8000i32;
                        for i in (1..=n).rev() {
                            let weight = i32::from(weights[(i - 1) as usize]);
                            acc += i32::from(src.get(j - n - i, y).wrapping_sub(center)) * weight
                                + i32::from(src.get(j - n + i, y).wrapping_sub(center)) * weight;
                        }
                        (i32::from(center) + (acc >> 16)) as i16
                    })
                    .collect()
            })
            .collect(),
    )
}

fn naive_shrink_horz(src: &Grid) -> Grid {
    Grid(
        (0..src.height() as isize)
            .map(|y| {
                (0..((src.width() + 5) >> 1) as isize)
                    .map(|j| {
                        let at = |k: isize| src.get(2 * j + k, y);
                        shrink_func(at(-4), at(-3), at(-2), at(-1), at(0), at(1))
                    })
                    .collect()
            })
            .collect(),
    )
}

fn naive_expand_horz(src: &Grid) -> Grid {
    Grid(
        (0..src.height() as isize)
            .map(|y| {
                let mut row = Vec::new();
                for m in 0..(src.width() as isize + 2) {
                    let (positive, negative) =
                        expand_func(src.get(m - 2, y), src.get(m - 1, y), src.get(m, y));
                    row.push(positive);
                    row.push(negative);
                }
                row
            })
            .collect(),
    )
}

/// Запускает ядро, пишущее новую плоскость `out`, на буфере со «грязным» приёмником
/// (проверяет, что ядро перезаписывает всё, а не полагается на нули).
fn run_kernel(out: Plane, kernel: impl FnOnce(&mut [i16])) -> Vec<i16> {
    let mut dst = vec![0x2A2Ai16; out.len()];
    kernel(&mut dst);
    dst
}

const KERNEL_SIZES: [(usize, usize); 8] = [
    (1, 1),
    (2, 3),
    (7, 5),
    (15, 4),
    (16, 4),
    (17, 9),
    (33, 6),
    (70, 11),
];

#[test]
fn horizontal_kernels_match_the_formulas_on_zero_extended_planes() {
    let weights = [14398i16, 6802, 1949, 339, 36, 150, 77, 5];
    for (seed, (width, height)) in KERNEL_SIZES.into_iter().enumerate() {
        let (plane, data, grid) = random_plane(width, height, 17 + seed as u64);
        let mut rows = RowScratch::default();

        let out = Plane {
            width: (width + 5) >> 1,
            height,
        };
        let dst = run_kernel(out, |dst| shrink_horz(dst, &data, plane, &mut rows));
        assert_eq!(
            plane_to_grid(out, &dst).0,
            naive_shrink_horz(&grid).0,
            "shrink_horz {width}x{height}"
        );

        let out = Plane {
            width: 2 * width + 4,
            height,
        };
        let dst = run_kernel(out, |dst| expand_horz(dst, &data, plane, &mut rows));
        assert_eq!(
            plane_to_grid(out, &dst).0,
            naive_expand_horz(&grid).0,
            "expand_horz {width}x{height}"
        );

        for radius in 4..=8 {
            let out = Plane {
                width: width + 2 * radius,
                height,
            };
            let dst = run_kernel(out, |dst| {
                blur_horz_dispatch(radius, dst, &data, plane, &weights, &mut rows)
            });
            assert_eq!(
                plane_to_grid(out, &dst).0,
                naive_blur_horz(&grid, radius, &weights).0,
                "blur_horz radius {radius} {width}x{height}"
            );
        }
    }
}

#[test]
fn vertical_kernels_equal_horizontal_ones_on_the_transposed_plane() {
    let weights = [9271i16, 7462, 5174, 3128, 1600, 757, 247, 139];
    for (seed, (width, height)) in KERNEL_SIZES.into_iter().enumerate() {
        let (plane, data, grid) = random_plane(width, height, 91 + seed as u64);
        let transposed = grid.transposed();
        let mut rows = RowScratch::default();

        let out = Plane {
            width,
            height: (height + 5) >> 1,
        };
        let dst = run_kernel(out, |dst| shrink_vert(dst, &data, plane, &mut rows));
        assert_eq!(
            plane_to_grid(out, &dst).0,
            naive_shrink_horz(&transposed).transposed().0,
            "shrink_vert {width}x{height}"
        );

        let out = Plane {
            width,
            height: 2 * height + 4,
        };
        let dst = run_kernel(out, |dst| expand_vert(dst, &data, plane, &mut rows));
        assert_eq!(
            plane_to_grid(out, &dst).0,
            naive_expand_horz(&transposed).transposed().0,
            "expand_vert {width}x{height}"
        );

        for radius in 4..=8 {
            let out = Plane {
                width,
                height: height + 2 * radius,
            };
            let dst = run_kernel(out, |dst| {
                blur_vert_dispatch(radius, dst, &data, plane, &weights, &mut rows)
            });
            assert_eq!(
                plane_to_grid(out, &dst).0,
                naive_blur_horz(&transposed, radius, &weights)
                    .transposed()
                    .0,
                "blur_vert radius {radius} {width}x{height}"
            );
        }
    }
}

#[test]
fn shrink_and_expand_preserve_flat_regions() {
    // Постоянное поле 0x4000 внутри картинки остаётся 0x4000 (ядра нормированы).
    let size = 40;
    let plane = Plane {
        width: size,
        height: size,
    };
    // Только видимая часть: шаг строки здесь равен ширине 48, правее 40 должны быть нули.
    let mut data = vec![0i16; plane.len()];
    for row in data.chunks_exact_mut(plane.stride()) {
        row[..size].fill(0x4000);
    }
    let mut rows = RowScratch::default();

    let shrunk = Plane {
        width: (size + 5) >> 1,
        height: size,
    };
    let dst = run_kernel(shrunk, |dst| shrink_horz(dst, &data, plane, &mut rows));
    let grid = plane_to_grid(shrunk, &dst);
    assert_eq!(grid.get(10, 7), 0x4000);

    let expanded = Plane {
        width: 2 * size + 4,
        height: size,
    };
    let dst = run_kernel(expanded, |dst| expand_horz(dst, &data, plane, &mut rows));
    let grid = plane_to_grid(expanded, &dst);
    assert_eq!(grid.get(40, 7), 0x4000);
}
