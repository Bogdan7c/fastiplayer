//! Векторы эталона плеера (`render-wgpu-video/src/bt2446c_reference.rs`): те же входы — те же
//! ожидаемые числа. Совпадение здесь значит, что узлы таблицы GLES2 посчитаны по той же формуле,
//! что рисует wgpu.

use super::*;

/// Допуск эталона плеера для сверки с опубликованными значениями (`MATRIX_TOLERANCE`).
const REFERENCE_TOLERANCE: f64 = 1e-5;

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= REFERENCE_TOLERANCE,
        "{what}: {actual} vs {expected}"
    );
}

fn assert_rgb_close(actual: [f64; 3], expected: [f64; 3], what: &str) {
    for (channel, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
        assert_close(actual, expected, &format!("{what} channel {channel}"));
    }
}

/// Закодированный SDR для линейного HDR-входа в нитах (векторы `convert_direct_linear_sample`).
fn encoded_from_linear(linear_hdr_nits: [f64; 3]) -> [f64; 3] {
    relative_sdr_from_linear_hdr(linear_hdr_nits, HdrReferenceLevels::REFERENCE_DEFAULT)
        .map(|relative| relative.clamp(0.0, 1.0).powf(1.0 / BT1886_REFERENCE_GAMMA))
}

/// Обратная PQ EOTF (ST 2084, уравнение 5.2): нит → код, чтобы подать вектор через R'G'B'.
fn pq_inverse_eotf(nits: f64) -> f64 {
    let normalized = (nits / PQ_REFERENCE_PEAK_NITS).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * normalized) / (1.0 + PQ_C3 * normalized)).powf(PQ_M2)
}

#[test]
fn pq_eotf_known_points_match_st2084() {
    assert_close(pq_eotf_nits(0.0), 0.0, "PQ 0");
    assert_close(pq_eotf_nits(0.508_078_421_517_399), 100.0, "PQ 100 nit");
    assert_close(pq_eotf_nits(0.751_827_096_247_041), 1_000.0, "PQ 1000 nit");
    assert_close(pq_eotf_nits(1.0), 10_000.0, "PQ peak");
}

#[test]
fn hlg_eotf_known_points_match_bt2100() {
    let achromatic = |code: f64| {
        linear_hdr_nits(
            [code; 3],
            HdrTransfer::Hlg,
            HdrReferenceLevels::REFERENCE_DEFAULT,
        )[1]
    };
    assert_close(achromatic(0.0), 0.0, "HLG 0");
    assert_close(achromatic(0.5), 50.697_028_491, "HLG 0.5");
    assert_close(achromatic(0.75), 203.152_145_938, "HLG 0.75");
    assert!((achromatic(1.0) - 1_000.0).abs() <= 1e-4, "HLG peak");
}

#[test]
fn neutral_and_colored_vectors_match_player_reference() {
    let cases = [
        ([100.0; 3], [0.879_130_957, 0.879_115_981, 0.879_099_531]),
        ([203.0; 3], [0.960_007_104, 0.959_990_750, 0.959_972_786]),
        ([1_000.0; 3], [1.0; 3]),
        ([203.0, 100.0, 50.0], [1.0, 0.832_203_632, 0.623_419_858]),
        ([203.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
    ];
    for (linear, expected) in cases {
        assert_rgb_close(
            encoded_from_linear(linear),
            expected,
            &format!("{linear:?}"),
        );
    }
}

#[test]
fn pq_and_hlg_codes_reach_the_reference_outputs() {
    let levels = HdrReferenceLevels::REFERENCE_DEFAULT;
    let pq_white = pq_inverse_eotf(203.0);
    assert_rgb_close(
        bt2446c_to_sdr([pq_white; 3], HdrTransfer::Pq, levels),
        [0.960_007_104, 0.959_990_750, 0.959_972_786],
        "PQ 203 nit",
    );
    assert_rgb_close(
        bt2446c_to_sdr([0.75; 3], HdrTransfer::Hlg, levels),
        [0.960_074_102, 0.960_057_747, 0.960_039_782],
        "HLG 0.75",
    );
}

#[test]
fn unclamped_node_value_equals_output_inside_range_and_clamps_to_it_outside() {
    let levels = HdrReferenceLevels::REFERENCE_DEFAULT;
    // Нейтраль 203 нит — внутри 0..1: узел равен выходу.
    let inside = [pq_inverse_eotf(203.0); 3];
    let node = unclamped_encoded_sdr(inside, HdrTransfer::Pq, levels);
    assert_rgb_close(
        node,
        bt2446c_to_sdr(inside, HdrTransfer::Pq, levels),
        "inside",
    );
    // Насыщенный красный: R выше 1, G и B ниже 0 до ограничения — после clamp тот же выход.
    let saturated_red = [pq_inverse_eotf(1_000.0), 0.0, 0.0];
    let node = unclamped_encoded_sdr(saturated_red, HdrTransfer::Pq, levels);
    assert!(node[0] > 1.0, "red above white before clamp: {node:?}");
    assert!(
        node[1] < 0.0 && node[2] < 0.0,
        "out-of-gamut negatives: {node:?}"
    );
    assert_rgb_close(
        node.map(|value| value.clamp(0.0, 1.0)),
        bt2446c_to_sdr(saturated_red, HdrTransfer::Pq, levels),
        "outside",
    );
}

#[test]
fn input_outside_unit_range_is_bounded_before_eotf() {
    let levels = HdrReferenceLevels::REFERENCE_DEFAULT;
    assert_eq!(
        bt2446c_to_sdr([-0.5, 1.5, 2.0], HdrTransfer::Pq, levels),
        bt2446c_to_sdr([0.0, 1.0, 1.0], HdrTransfer::Pq, levels)
    );
}

#[test]
fn hlg_peak_follows_levels_but_pq_does_not() {
    let bright = HdrReferenceLevels::new(100.0, 2_000.0).expect("valid levels");
    let base = HdrReferenceLevels::REFERENCE_DEFAULT;
    let code = [0.6; 3];
    assert_ne!(
        bt2446c_to_sdr(code, HdrTransfer::Hlg, bright),
        bt2446c_to_sdr(code, HdrTransfer::Hlg, base),
        "HLG display peak changes the output"
    );
    assert_eq!(
        bt2446c_to_sdr(code, HdrTransfer::Pq, bright),
        bt2446c_to_sdr(code, HdrTransfer::Pq, base),
        "PQ is absolute: peak setting is not part of the formula (as in wgpu)"
    );
}
