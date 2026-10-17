//! Проверка заливки тайла на предельных коэффициентах против «прямолинейной» эталонной
//! реализации: дословная транскрипция C (`c/rasterizer_template.h`), где КАЖДОЕ присваивание
//! в `int16_t` явно усекается `as i16`, а промежуточные значения считаются в `i32`/`i64`, как
//! `int`/`int64_t` в C. Боевой код считает часть цепочек сразу в `i16` с `wrapping_*`
//! (автовекторизация); тест фиксирует, что это даёт те же байты, в том числе там, где
//! 16-битные значения переполняются.

use super::segment::Segment;
use super::tile::{TILE_SIZE, fill_generic_tile, fill_halfplane_tile};
use crate::outline::{OUTLINE_MAX, Point};

const TILE_ORDER: u32 = 4;
const FULL_VALUE: i32 = 1 << (14 - TILE_ORDER);
const SIZE: usize = TILE_SIZE;

/// `RESCALE_AB` с присваиванием в `int16_t`.
fn ref_rescale_ab(ab: i32, scale: i32) -> i16 {
    ((i64::from(ab) * i64::from(scale) + (1_i64 << (45 + TILE_ORDER))) >> (46 + TILE_ORDER)) as i16
}

/// `RESCALE_C` (результат `int64_t`, усечения нет).
fn ref_rescale_c(c: i64, scale: i32) -> i64 {
    ((i64::from((c >> (7 + TILE_ORDER)) as i32) * i64::from(scale)) + (1_i64 << 44)) >> 45
}

fn clamp16(value: i16, low: i32, high: i32) -> i16 {
    i32::from(value).clamp(low, high) as i16
}

/// `ass_fill_halfplane_tile` из C.
fn ref_halfplane(a: i32, b: i32, c: i64, scale: i32) -> Vec<u8> {
    let aa = ref_rescale_ab(a, scale);
    let bb = ref_rescale_ab(b, scale);
    let mut cc = (ref_rescale_c(c, scale) + i64::from(FULL_VALUE / 2)
        - i64::from((i32::from(aa) + i32::from(bb)) >> 1)) as i16;
    let abs_a = (-i32::from(aa)).max(i32::from(aa)) as i16;
    let abs_b = (-i32::from(bb)).max(i32::from(bb)) as i16;
    let delta = ((i32::from(abs_a.min(abs_b)) + 2) >> 2) as i16;
    let mut va1 = [0_i16; SIZE];
    let mut va2 = [0_i16; SIZE];
    for x in 0..SIZE {
        va1[x] = (i32::from(aa) * x as i32 - i32::from(delta)) as i16;
        va2[x] = (i32::from(aa) * x as i32 + i32::from(delta)) as i16;
    }
    let mut out = Vec::with_capacity(SIZE * SIZE);
    for _ in 0..SIZE {
        for x in 0..SIZE {
            let c1 = (i32::from(cc) - i32::from(va1[x])) as i16;
            let c2 = (i32::from(cc) - i32::from(va2[x])) as i16;
            let c1 = clamp16(c1, 0, FULL_VALUE);
            let c2 = clamp16(c2, 0, FULL_VALUE);
            let res = ((i32::from(c1) + i32::from(c2)) >> (7 - TILE_ORDER)) as i16;
            out.push(res.min(255) as u8);
        }
        cc = (i32::from(cc) - i32::from(bb)) as i16;
    }
    out
}

/// `update_border_line` из C.
#[allow(clippy::too_many_arguments)]
fn ref_update_border_line(
    res: &mut [i16; SIZE],
    abs_a: i16,
    va: &[i16; SIZE],
    b: i16,
    abs_b: i16,
    c: i16,
    up: i32,
    dn: i32,
) {
    let size = (dn - up) as i16;
    let mut w = (FULL_VALUE + (i32::from(size) << (8 - TILE_ORDER)) - i32::from(abs_a)) as i16;
    w = ((i32::from(w).min(FULL_VALUE)) << (2 * TILE_ORDER - 5)) as i16;
    let dc_b = ((i32::from(abs_b) * i32::from(size)) >> 6) as i16;
    let dc = ((i32::from(abs_a.min(dc_b)) + 2) >> 2) as i16;
    let base = ((i32::from(b) * i32::from((up + dn) as i16)) >> 7) as i16;
    // В C `(base + dc) * (int32_t) w`: сумма считается в `int` без усечения до int16.
    let offs = |sum: i32| (i32::from(size) - (sum.wrapping_mul(i32::from(w)) >> 16)) as i16;
    let offs1 = offs(i32::from(base) + i32::from(dc));
    let offs2 = offs(i32::from(base) - i32::from(dc));
    let size = (i32::from(size) << 1) as i16;
    for x in 0..SIZE {
        let diff = i32::from(c) - i32::from(va[x]);
        let cw = (diff.wrapping_mul(i32::from(w)) >> 16) as i16;
        let c1 = (i32::from(cw) + i32::from(offs1)) as i16;
        let c2 = (i32::from(cw) + i32::from(offs2)) as i16;
        let c1 = clamp16(c1, 0, i32::from(size));
        let c2 = clamp16(c2, 0, i32::from(size));
        res[x] = (i32::from(res[x]) + i32::from(c1) + i32::from(c2)) as i16;
    }
}

/// `ass_fill_generic_tile` из C.
fn ref_generic(lines: &[Segment], winding: i32) -> Vec<u8> {
    let mut res = [[0_i16; SIZE]; SIZE];
    let mut delta = [0_i16; SIZE + 2];
    for line in lines {
        let mut up_delta: i16 = if line.flags & super::segment::SEGFLAG_DN != 0 {
            4
        } else {
            0
        };
        let mut dn_delta = up_delta;
        if line.x_min == 0 && line.flags & super::segment::SEGFLAG_EXACT_LEFT != 0 {
            dn_delta ^= 4;
        }
        if line.flags & super::segment::SEGFLAG_UL_DR != 0 {
            std::mem::swap(&mut up_delta, &mut dn_delta);
        }
        let mut up = (line.y_min >> 6) as usize;
        let dn = (line.y_max >> 6) as usize;
        let up_pos = (line.y_min & 63) as i16;
        let up_delta1 = (i32::from(up_delta) * i32::from(up_pos)) as i16;
        let dn_pos = (line.y_max & 63) as i16;
        let dn_delta1 = (i32::from(dn_delta) * i32::from(dn_pos)) as i16;
        let sub = |d: &mut i16, v: i32| *d = (i32::from(*d) - v) as i16;
        let add = |d: &mut i16, v: i32| *d = (i32::from(*d) + v) as i16;
        sub(&mut delta[up + 1], i32::from(up_delta1));
        sub(
            &mut delta[up],
            (i32::from(up_delta) << 6) - i32::from(up_delta1),
        );
        add(&mut delta[dn + 1], i32::from(dn_delta1));
        add(
            &mut delta[dn],
            (i32::from(dn_delta) << 6) - i32::from(dn_delta1),
        );
        if line.y_min == line.y_max {
            continue;
        }
        let a = ref_rescale_ab(line.a, line.scale);
        let b = ref_rescale_ab(line.b, line.scale);
        let mut c = (ref_rescale_c(line.c, line.scale)
            - i64::from(i32::from(a) >> 1)
            - i64::from(i32::from(b) * up as i32)) as i16;
        let mut va = [0_i16; SIZE];
        for (x, cell) in va.iter_mut().enumerate() {
            *cell = (i32::from(a) * x as i32) as i16;
        }
        let abs_a = (-i32::from(a)).max(i32::from(a)) as i16;
        let abs_b = (-i32::from(b)).max(i32::from(b)) as i16;
        let dc = ((i32::from(abs_a.min(abs_b)) + 2) >> 2) as i16;
        let base = (FULL_VALUE / 2 - (i32::from(b) >> 1)) as i16;
        let dc1 = (i32::from(base) + i32::from(dc)) as i16;
        let dc2 = (i32::from(base) - i32::from(dc)) as i16;
        if up_pos != 0 {
            if dn == up {
                ref_update_border_line(
                    &mut res[up],
                    abs_a,
                    &va,
                    b,
                    abs_b,
                    c,
                    i32::from(up_pos),
                    i32::from(dn_pos),
                );
                continue;
            }
            ref_update_border_line(&mut res[up], abs_a, &va, b, abs_b, c, i32::from(up_pos), 64);
            up += 1;
            c = (i32::from(c) - i32::from(b)) as i16;
        }
        for row in res.iter_mut().take(dn).skip(up) {
            for x in 0..SIZE {
                let c1 = (i32::from(c) - i32::from(va[x]) + i32::from(dc1)) as i16;
                let c2 = (i32::from(c) - i32::from(va[x]) + i32::from(dc2)) as i16;
                let c1 = clamp16(c1, 0, FULL_VALUE);
                let c2 = clamp16(c2, 0, FULL_VALUE);
                row[x] = (i32::from(row[x]) + ((i32::from(c1) + i32::from(c2)) >> (7 - TILE_ORDER)))
                    as i16;
            }
            c = (i32::from(c) - i32::from(b)) as i16;
        }
        if dn_pos != 0 {
            ref_update_border_line(&mut res[dn], abs_a, &va, b, abs_b, c, 0, i32::from(dn_pos));
        }
    }
    let mut cur = (256 * i32::from(winding as i8)) as i16;
    let mut out = Vec::with_capacity(SIZE * SIZE);
    for (row, row_delta) in res.iter().zip(&delta) {
        cur = (i32::from(cur) + i32::from(*row_delta)) as i16;
        for cell in row {
            let val = (i32::from(*cell) + i32::from(cur)) as i16;
            let neg = (-i32::from(val)) as i16;
            let val = if val > neg { val } else { neg };
            out.push(val.min(255) as u8);
        }
    }
    out
}

/// Предельные отрезки: самые длинные из допустимых (`OUTLINE_MAX`), почти горизонтальные,
/// почти вертикальные, диагонали обоих знаков и обычные. После `Segment::from_line`
/// `max(|a|, |b|)` лежит в 2^30..2^31, а `scale` - наибольший из законных (~2^30 при самом
/// малом `max_ab`), то есть именно здесь 16-битное усечение коэффициентов ближе всего к границе.
fn extreme_segments() -> Vec<Segment> {
    let max = OUTLINE_MAX;
    let directions = [
        (max, 1),
        (1, max),
        (-max, 1),
        (1, -max),
        (max, max),
        (-max, max),
        (max, -max),
        (1000, 37),
        (-3, 200_000),
        (1, 1),
        (64, 0),
        (0, -64),
    ];
    directions
        .into_iter()
        .filter_map(|(dx, dy)| {
            // Начало подобрано так, чтобы прямая проходила через середину тайла (512, 512).
            let start = Point::new(512 - dx / 2, 512 - dy / 2);
            let end = Point::new(start.x + dx, start.y + dy);
            let mut segment = Segment::from_line(start, end)?;
            // Окно сегмента - сам тайл: сегмент целиком внутри по Y, как требует заливка.
            segment.x_min = 0;
            segment.x_max = 1024;
            segment.y_min = 0;
            segment.y_max = 1024;
            Some(segment)
        })
        .collect()
}

#[test]
fn extreme_segments_use_the_largest_legal_scale_and_fit_int16_after_rescaling() {
    let segments = extreme_segments();
    assert!(segments.len() >= 10);
    // Наибольший законный `scale` действительно встречается: максимальный коэффициент
    // нормализован к нижней границе диапазона, где `scale` максимален.
    assert!(segments.iter().any(|segment| segment.scale > 1 << 29));
    for segment in &segments {
        for coefficient in [segment.a, segment.b] {
            let wide = (i64::from(coefficient) * i64::from(segment.scale)
                + (1_i64 << (45 + TILE_ORDER)))
                >> (46 + TILE_ORDER);
            assert!(
                i16::try_from(wide).is_ok(),
                "{coefficient} * {} -> {wide}",
                segment.scale
            );
        }
    }
}

#[test]
fn halfplane_tile_matches_the_int16_narrowing_reference_on_extreme_coefficients() {
    for segment in extreme_segments() {
        let mut buf = vec![0xAA_u8; SIZE * SIZE];
        fill_halfplane_tile(
            &mut buf,
            0,
            SIZE,
            segment.a,
            segment.b,
            segment.c,
            segment.scale,
        )
        .expect("tile fits the buffer");
        let expected = ref_halfplane(segment.a, segment.b, segment.c, segment.scale);
        assert_eq!(buf, expected, "{segment:?}");
        // Не вырожденный случай: тайл не залит одним значением.
        assert!(
            buf.iter().any(|&v| v != buf[0]),
            "the edge crosses the tile for {segment:?}"
        );
    }
}

#[test]
fn generic_tile_matches_the_int16_narrowing_reference_on_extreme_coefficients() {
    let segments = extreme_segments();
    // Каждый отрезок отдельно, затем все вместе (накопление `res` и `delta` в int16).
    let mut cases: Vec<Vec<Segment>> = segments.iter().map(|segment| vec![*segment]).collect();
    cases.push(segments.clone());
    // Отрезки с неполными верхней/нижней строками (границы не по линиям сетки).
    cases.push(
        segments
            .iter()
            .map(|segment| Segment {
                y_min: 100,
                y_max: 900,
                ..*segment
            })
            .collect(),
    );
    for winding in [0, 1, -1] {
        for lines in &cases {
            let mut buf = vec![0xAA_u8; SIZE * SIZE];
            fill_generic_tile(&mut buf, 0, SIZE, lines, winding).expect("tile fits the buffer");
            assert_eq!(buf, ref_generic(lines, winding), "winding {winding}");
        }
    }
}

/// Детерминированный набор из сотен сегментов со случайными наклонами, длинами и окнами по Y
/// (линейный конгруэнтный генератор с фиксированным зерном): ловит расхождения в ветках
/// `update_border_line` и в средних строках, которые предельные случаи не задевают.
fn swept_segments() -> Vec<Segment> {
    let mut state = 0x1234_5678_9ABC_DEF0_u64;
    let mut next = move |modulus: i64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((state >> 33) as i64) % modulus
    };
    let mut segments = Vec::new();
    while segments.len() < 400 {
        // Длина от единиц до OUTLINE_MAX: порядок величины выбирается случайно.
        let magnitude = 1_i64 << (next(28) + 1);
        let dx = (next(2 * magnitude) - magnitude) as i32;
        let dy = (next(2 * magnitude) - magnitude) as i32;
        let start = Point::new(512 - dx / 2, 512 - dy / 2);
        let end = Point::new(start.x + dx, start.y + dy);
        let Some(mut segment) = Segment::from_line(start, end) else {
            continue;
        };
        let y_min = next(1024) as i32;
        let y_max = y_min + next(1025 - i64::from(y_min)) as i32;
        segment.x_min = next(900) as i32;
        segment.x_max = 1024;
        segment.y_min = y_min;
        segment.y_max = y_max.max(y_min);
        segments.push(segment);
    }
    segments
}

#[test]
fn swept_segments_match_the_int16_narrowing_reference() {
    let segments = swept_segments();
    for segment in &segments {
        let mut buf = vec![0xAA_u8; SIZE * SIZE];
        fill_halfplane_tile(
            &mut buf,
            0,
            SIZE,
            segment.a,
            segment.b,
            segment.c,
            segment.scale,
        )
        .expect("tile fits the buffer");
        assert_eq!(
            buf,
            ref_halfplane(segment.a, segment.b, segment.c, segment.scale),
            "{segment:?}"
        );
        let single = [*segment];
        let mut generic = vec![0xAA_u8; SIZE * SIZE];
        fill_generic_tile(&mut generic, 0, SIZE, &single, 0).expect("tile fits the buffer");
        assert_eq!(generic, ref_generic(&single, 0), "{segment:?}");
    }
    // Пачками по десять: накопление нескольких отрезков в `res` и `delta`.
    for batch in segments.chunks(10) {
        let mut generic = vec![0xAA_u8; SIZE * SIZE];
        fill_generic_tile(&mut generic, 0, SIZE, batch, 1).expect("tile fits the buffer");
        assert_eq!(generic, ref_generic(batch, 1));
    }
}
