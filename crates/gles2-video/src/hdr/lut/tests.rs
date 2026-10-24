//! Таблица против точной формулы: та же интерполяция, что делает шейдер (билинейная в срезе +
//! смешивание соседних срезов), на CPU. Допуск — документированный в модуле `hdr`.

use super::*;
use crate::hdr::bt2446c::signed_inverse_bt1886;
use crate::hdr::bt2446c::{BT2020_KB, BT2020_KG, BT2020_KR};
use crate::hdr::bt2446c_to_sdr;

/// Худшее допустимое отличие от точной формулы, в шагах 1/255 (модуль `hdr`).
const MAX_DIFFERENCE: u8 = 3;
/// Доля отсчётов с отличием больше 1/255, не больше (модуль `hdr`: 0,1–0,3 %).
const MAX_SHARE_ABOVE_ONE: f64 = 0.005;

fn texel(table: &HdrLutTable, x: u32, y: u32) -> [f64; 3] {
    let offset = (y as usize * atlas_width() as usize + x as usize) * BYTES_PER_TEXEL;
    let [scale, bias] = decode_scale_offset().map(f64::from);
    [0, 1, 2].map(|channel| f64::from(table.rgba[offset + channel]) / 255.0 * scale + bias)
}

/// То, что делает шейдер: координата узла, билинейная выборка в двух срезах, смешивание по B,
/// `clamp`.
fn interpolate(table: &HdrLutTable, rgb: [f64; 3]) -> [f64; 3] {
    let last = f64::from(NODES_PER_AXIS - 1);
    let position = rgb.map(|value| value.clamp(0.0, 1.0) * last);
    let base = position.map(|value| value.floor().min(last - 1.0));
    let weight = [0, 1, 2].map(|axis| position[axis] - base[axis]);
    let index = base.map(|value| value as u32);
    let mut result = [0.0; 3];
    for (blue_step, blue_weight) in [(0, 1.0 - weight[2]), (1, weight[2])] {
        for (green_step, green_weight) in [(0, 1.0 - weight[1]), (1, weight[1])] {
            for (red_step, red_weight) in [(0, 1.0 - weight[0]), (1, weight[0])] {
                let (x, y) = node_texel(
                    index[0] + red_step,
                    index[1] + green_step,
                    index[2] + blue_step,
                );
                let node = texel(table, x, y);
                let share = red_weight * green_weight * blue_weight;
                for (out, value) in result.iter_mut().zip(node) {
                    *out += share * value;
                }
            }
        }
    }
    result.map(|value| value.clamp(0.0, 1.0))
}

fn to_byte(value: f64) -> i32 {
    (value * 255.0).round() as i32
}

/// R'G'B' BT.2020 из 10-битных limited кодов.
fn rgb_from_limited_10bit(y: u32, u: u32, v: u32) -> [f64; 3] {
    let luma = (f64::from(y) - 64.0) / 876.0;
    let cb = (f64::from(u) - 512.0) / 896.0;
    let cr = (f64::from(v) - 512.0) / 896.0;
    let red = luma + 2.0 * (1.0 - BT2020_KR) * cr;
    let blue = luma + 2.0 * (1.0 - BT2020_KB) * cb;
    let green = (luma - BT2020_KR * red - BT2020_KB * blue) / BT2020_KG;
    [red, green, blue]
}

fn assert_table_matches_formula(transfer: HdrTransfer) {
    let levels = HdrReferenceLevels::REFERENCE_DEFAULT;
    let table = HdrLutTable::build(transfer, levels);
    let mut samples = 0_u32;
    let mut above_one = 0_u32;
    let mut worst = 0;
    for y in (64..=940).step_by(6) {
        for u in (64..=960).step_by(28) {
            for v in (64..=960).step_by(28) {
                let rgb = rgb_from_limited_10bit(y, u, v);
                let exact = bt2446c_to_sdr(rgb, transfer, levels);
                let table_value = interpolate(&table, rgb);
                let difference = (0..3)
                    .map(|channel| (to_byte(exact[channel]) - to_byte(table_value[channel])).abs())
                    .max()
                    .unwrap_or(0);
                worst = worst.max(difference);
                above_one += u32::from(difference > 1);
                samples += 1;
            }
        }
    }
    let share = f64::from(above_one) / f64::from(samples);
    assert!(
        worst <= i32::from(MAX_DIFFERENCE),
        "{transfer:?}: worst {worst}/255 over {samples} samples"
    );
    assert!(
        share <= MAX_SHARE_ABOVE_ONE,
        "{transfer:?}: {:.3} % of samples differ by more than 1/255",
        share * 100.0
    );
}

#[test]
fn pq_table_matches_exact_formula_within_documented_tolerance() {
    assert_table_matches_formula(HdrTransfer::Pq);
}

#[test]
fn hlg_table_matches_exact_formula_within_documented_tolerance() {
    assert_table_matches_formula(HdrTransfer::Hlg);
}

#[test]
fn nodes_reproduce_the_formula_at_grid_points() {
    // В самих узлах интерполяции нет: отличие только от 8 бит хранения (полшага 1,3/255).
    let levels = HdrReferenceLevels::REFERENCE_DEFAULT;
    let table = HdrLutTable::build(HdrTransfer::Pq, levels);
    for (red, green, blue) in [
        (0, 0, 0),
        (64, 64, 64),
        (32, 40, 8),
        (64, 0, 0),
        (10, 50, 64),
    ] {
        let rgb = [red, green, blue].map(node_code);
        let exact = bt2446c_to_sdr(rgb, HdrTransfer::Pq, levels);
        let (x, y) = node_texel(red, green, blue);
        let stored = texel(&table, x, y).map(|value| value.clamp(0.0, 1.0));
        for channel in 0..3 {
            assert!(
                (stored[channel] - exact[channel]).abs() <= 1.3 / 255.0,
                "node ({red},{green},{blue}) channel {channel}: {} vs {}",
                stored[channel],
                exact[channel]
            );
        }
    }
}

#[test]
fn atlas_fits_common_texture_limits_and_nodes_do_not_overlap() {
    assert_eq!((atlas_width(), atlas_height()), (520, 585));
    let mut seen = std::collections::HashSet::new();
    for blue in 0..NODES_PER_AXIS {
        for green in [0, NODES_PER_AXIS - 1] {
            for red in [0, NODES_PER_AXIS - 1] {
                let (x, y) = node_texel(red, green, blue);
                assert!(x < atlas_width() && y < atlas_height());
                assert!(seen.insert((x, y)), "node ({red},{green},{blue}) overlaps");
            }
        }
    }
}

#[test]
fn stored_byte_covers_the_encoded_range_with_rounding() {
    assert_eq!(store_byte(ENCODED_MIN - 1.0), 0);
    assert_eq!(store_byte(ENCODED_MIN), 0);
    assert_eq!(store_byte(ENCODED_MAX), 255);
    assert_eq!(store_byte(ENCODED_MAX + 3.0), 255);
    // 0 и 1 — ровно те значения, которые важно не сдвинуть после clamp: 0 и 1 после декодирования
    // ближе полшага к своему узлу.
    let [scale, bias] = decode_scale_offset().map(f64::from);
    for value in [0.0, 1.0] {
        let decoded = f64::from(store_byte(value)) / 255.0 * scale + bias;
        assert!((decoded - value).abs() <= 0.5 * scale / 255.0 + 1e-9);
    }
}

#[test]
fn transfer_and_levels_change_the_table() {
    let base = HdrReferenceLevels::REFERENCE_DEFAULT;
    let brighter_white = HdrReferenceLevels::new(200.0, 1_000.0).expect("valid levels");
    let pq = HdrLutTable::build(HdrTransfer::Pq, base);
    assert!(pq.rgba != HdrLutTable::build(HdrTransfer::Hlg, base).rgba);
    assert!(pq.rgba != HdrLutTable::build(HdrTransfer::Pq, brighter_white).rgba);
    assert_ne!(
        HdrLutKey::new(HdrTransfer::Pq, base),
        HdrLutKey::new(HdrTransfer::Pq, brighter_white)
    );
    assert_eq!(
        HdrLutKey::new(HdrTransfer::Hlg, base),
        HdrLutKey::new(HdrTransfer::Hlg, base)
    );
}

/// Замер времени построения таблицы (не проверка): `cargo test --release -p gles2-video
/// table_build_time -- --ignored --nocapture`.
#[test]
#[ignore = "timing probe, run explicitly"]
fn table_build_time() {
    let levels = HdrReferenceLevels::REFERENCE_DEFAULT;
    for transfer in [HdrTransfer::Pq, HdrTransfer::Hlg] {
        let started = std::time::Instant::now();
        let runs = 5;
        for _ in 0..runs {
            std::hint::black_box(HdrLutTable::build(transfer, levels));
        }
        eprintln!(
            "{transfer:?}: {:.1} ms per table",
            started.elapsed().as_secs_f64() * 1000.0 / f64::from(runs)
        );
    }
}

/// Поиск по границам даёт тот же байт, что гамма + округление, во всём хранимом диапазоне и за
/// ним (кроме значений в 1e-9 от самой границы — там решает округление float).
#[test]
fn byte_thresholds_match_gamma_and_rounding() {
    let thresholds = ByteThresholds::new();
    let mut checked = 0;
    for step in -2_000..=40_000 {
        let relative = f64::from(step) * 1e-4;
        let expected = store_byte(signed_inverse_bt1886(relative));
        let near_boundary = thresholds
            .linear
            .iter()
            .any(|threshold| (threshold - relative).abs() < 1e-9);
        if !near_boundary {
            assert_eq!(
                thresholds.byte_for_relative_linear(relative),
                expected,
                "relative {relative}"
            );
            checked += 1;
        }
    }
    assert!(checked > 40_000);
}
