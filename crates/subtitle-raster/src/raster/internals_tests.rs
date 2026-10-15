//! Тесты внутренних частей растеризатора: сегменты, разбиение, заливка тайла.

use super::segment::{
    SEGFLAG_DN, SEGFLAG_UL_DR, Segment, SplitCounts, SplitLine, floor_log2, split_segments,
};
use super::tile::{fill_halfplane_tile, fill_solid_tile};
use super::{RasterError, TILE_SIZE};
use crate::outline::Point;

#[test]
fn segment_halfplane_is_normalized_and_oriented() {
    let down = Segment::from_line(Point::new(0, 0), Point::new(0, 64)).unwrap();
    assert!(down.flags & SEGFLAG_DN != 0);
    let up = Segment::from_line(Point::new(0, 64), Point::new(0, 0)).unwrap();
    assert!(up.flags & SEGFLAG_DN == 0);
    assert!(Segment::from_line(Point::new(5, 5), Point::new(5, 5)).is_none());

    // Нормализация: max(|a|, |b|) * scale ~ 2^60 для любых наклонов и длин
    // (в комментарии libass «2^61» относится к удвоенному max, которым считается scale).
    // Точное деление libass заменяет полиномом (строка-комментарий `(1 << 61) / max_ab` в ass_rasterizer.c), ошибка до ~2%.
    for (dx, dy) in [
        (64, 0),
        (0, -64),
        (1, 1),
        (1000, 37),
        (-123_456, 7),
        (268_435_455, -268_435_455),
        (3, 200_000),
    ] {
        let line = Segment::from_line(Point::new(10, 20), Point::new(10 + dx, 20 + dy)).unwrap();
        let max_ab = i128::from(line.a.unsigned_abs().max(line.b.unsigned_abs()));
        let product = max_ab * i128::from(line.scale);
        let target = 1_i128 << 60;
        let relative = (product - target).abs() as f64 / target as f64;
        assert!(relative < 0.04, "({dx}, {dy}): relative error {relative}");
        assert!(
            max_ab >= 1_i128 << 30,
            "coefficients are scaled up to 2^30..2^31"
        );
    }
    // Диагональ из левого верхнего в правый нижний угол помечена UL_DR.
    let diagonal = Segment::from_line(Point::new(0, 0), Point::new(64, 64)).unwrap();
    assert!(diagonal.flags & SEGFLAG_UL_DR != 0);
}

#[test]
fn splitting_a_diagonal_segment_cuts_it_at_the_line() {
    let diagonal = Segment::from_line(Point::new(0, 0), Point::new(128, 128)).unwrap();
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut winding = [0, 0];
    let counts = split_segments(
        &[diagonal],
        [1, 0],
        SplitLine::Vertical(64),
        &mut before,
        &mut after,
        &mut winding,
    );
    assert_eq!(
        counts,
        SplitCounts {
            before: [1, 0],
            after: [1, 0]
        }
    );
    assert_eq!((before[0].x_min, before[0].x_max), (0, 64));
    assert_eq!(
        (after[0].x_min, after[0].x_max),
        (0, 64),
        "second part is shifted to the line"
    );
    assert_eq!((after[0].y_min, after[0].y_max), (0, 128));
}

#[test]
fn splitting_distributes_whole_segments_and_counts_groups() {
    let left = Segment::from_line(Point::new(10, 0), Point::new(10, 100)).unwrap();
    let right = Segment::from_line(Point::new(300, 0), Point::new(300, 100)).unwrap();
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut winding = [0, 0];
    // Первая группа: left; вторая: right, left.
    let counts = split_segments(
        &[left, right, left],
        [1, 2],
        SplitLine::Vertical(128),
        &mut before,
        &mut after,
        &mut winding,
    );
    assert_eq!(
        counts,
        SplitCounts {
            before: [1, 1],
            after: [0, 1]
        }
    );
    assert_eq!(after[0].x_min, 300 - 128);
}

#[test]
fn floor_log2_matches_the_integer_logarithm() {
    assert_eq!(floor_log2(0), 0);
    assert_eq!(floor_log2(1), 0);
    assert_eq!(floor_log2(2), 1);
    assert_eq!(floor_log2(31), 4);
    assert_eq!(floor_log2(32), 5);
    assert_eq!(floor_log2(u32::MAX), 31);
}

#[test]
fn solid_tile_fill_writes_only_its_sixteen_rows() {
    let mut buffer = vec![9_u8; 40 * 20];
    fill_solid_tile(&mut buffer, 3 + 2 * 40, 40, true).unwrap();
    for y in 0..20 {
        for x in 0..40 {
            let inside = (3..19).contains(&x) && (2..18).contains(&y);
            assert_eq!(
                buffer[y * 40 + x],
                if inside { 255 } else { 9 },
                "({x}, {y})"
            );
        }
    }
    fill_solid_tile(&mut buffer, 3 + 2 * 40, 40, false).unwrap();
    assert_eq!(buffer[2 * 40 + 3], 0);
    // Тайл не помещается в буфер.
    assert_eq!(
        fill_solid_tile(&mut buffer, 40 * 20 - 5, 40, true),
        Err(RasterError::BufferTooSmall)
    );
}

#[test]
fn halfplane_tile_fills_one_side_with_antialiasing() {
    // Граница x = 8 px: слева 255, справа 0. a = 64 * 2^25 ..., берём из настоящего отрезка.
    let mut segment =
        Segment::from_line(Point::new(8 * 64, 16 * 64), Point::new(8 * 64, 0)).unwrap();
    segment.flags |= SEGFLAG_DN;
    let mut buffer = vec![0_u8; TILE_SIZE * TILE_SIZE];
    for &scale in &[segment.scale, segment.scale.wrapping_neg()] {
        fill_halfplane_tile(
            &mut buffer,
            0,
            TILE_SIZE,
            segment.a,
            segment.b,
            segment.c,
            scale,
        )
        .unwrap();
        let left = buffer[5 * TILE_SIZE + 2];
        let right = buffer[5 * TILE_SIZE + 13];
        assert!(
            (left == 255 && right == 0) || (left == 0 && right == 255),
            "sides are solid and opposite: {left} / {right}"
        );
        assert_ne!(left, right);
    }
}
