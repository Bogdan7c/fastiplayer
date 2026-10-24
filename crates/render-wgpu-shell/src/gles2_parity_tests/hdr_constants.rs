//! Константы BT.2446-C у GLES2 = константы шейдеров wgpu (сессия 06).
//!
//! У GLES2 формула живёт не в шейдере, а в CPU-коде таблицы (`gles2_video::bt2446c_constants`),
//! поэтому сверяется он, а не GLSL. Источник истины wgpu — текст обоих его HDR-шейдеров; их
//! совпадение с CPU-эталоном плеера `bt2446c_reference` уже проверяет тест
//! `shader_bt2446c_constants_match_cpu_reference_constants` в `render-wgpu-video`. Разбор —
//! тот же приём: строка `const NAME: f32 = …;`.

use gles2_video::bt2446c_constants as gles2;

const HOST_16BIT_SHADER: &str =
    include_str!("../../../render-wgpu-video/shaders/host_yuv420_16bit_to_sdr.wgsl");
const P010_SHADER: &str =
    include_str!("../../../render-wgpu-video/shaders/p010_bt2446c_to_sdr.wgsl");

/// Константы WGSL в `f32`: сравнение с допуском половины шага `f32` около значения.
const F32_TOLERANCE: f64 = 1e-6;

fn const_value(shader: &str, name: &str, kind: &str) -> String {
    let prefix = format!("const {name}: {kind} = ");
    let line = shader
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("shader has no `{prefix}…`"));
    line.trim_start_matches(&prefix)
        .trim_end_matches(';')
        .replace('_', "")
}

fn f32_const(shader: &str, name: &str) -> f64 {
    let text = const_value(shader, name, "f32");
    text.parse()
        .unwrap_or_else(|error| panic!("`{name}` = `{text}` is not a number: {error}"))
}

fn vec3_const(shader: &str, name: &str) -> [f64; 3] {
    let text = const_value(shader, name, "vec3<f32>");
    let inner = text.trim_start_matches("vec3<f32>(").trim_end_matches(')');
    let values: Vec<f64> = inner
        .split(',')
        .map(|value| value.trim().parse().expect("vec3 component is a number"))
        .collect();
    values.try_into().expect("vec3 has three components")
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= F32_TOLERANCE * expected.abs().max(1.0),
        "{what}: GLES2 {actual} vs wgpu {expected}"
    );
}

#[test]
fn gles2_bt2446c_constants_match_both_wgpu_hdr_shaders() {
    let scalars = [
        ("BT2020_KR", gles2::BT2020_KR),
        ("BT2020_KB", gles2::BT2020_KB),
        ("BT2020_KG", gles2::BT2020_KG),
        ("PQ_M1", gles2::PQ_M1),
        ("PQ_M2", gles2::PQ_M2),
        ("PQ_C1", gles2::PQ_C1),
        ("PQ_C2", gles2::PQ_C2),
        ("PQ_C3", gles2::PQ_C3),
        ("PQ_REFERENCE_PEAK_NITS", gles2::PQ_REFERENCE_PEAK_NITS),
        ("HLG_A", gles2::HLG_A),
        ("HLG_B", gles2::HLG_B),
        ("HLG_C", gles2::HLG_C),
        (
            "HLG_REFERENCE_SYSTEM_GAMMA",
            gles2::HLG_REFERENCE_SYSTEM_GAMMA,
        ),
        (
            "BT2446C_REFERENCE_CROSSTALK_ALPHA",
            gles2::BT2446C_CROSSTALK_ALPHA,
        ),
        ("BT2446C_K1", gles2::BT2446C_K1),
        ("BT2446C_K2", gles2::BT2446C_K2),
        ("BT2446C_K3", gles2::BT2446C_K3),
        ("BT2446C_K4", gles2::BT2446C_K4),
        (
            "BT2446C_SDR_INFLECTION_NITS",
            gles2::BT2446C_SDR_INFLECTION_NITS,
        ),
        ("BT1886_REFERENCE_GAMMA", gles2::BT1886_REFERENCE_GAMMA),
    ];
    let matrices = [
        ("BT2020_LINEAR_RGB_TO_XYZ", gles2::BT2020_LINEAR_RGB_TO_XYZ),
        ("XYZ_TO_BT709_LINEAR_RGB", gles2::XYZ_TO_BT709_LINEAR_RGB),
    ];
    for (shader_name, shader) in [("host 16-bit", HOST_16BIT_SHADER), ("P010", P010_SHADER)] {
        for (name, value) in scalars {
            assert_close(
                value,
                f32_const(shader, name),
                &format!("{shader_name} {name}"),
            );
        }
        for (name, rows) in matrices {
            for (index, row) in rows.into_iter().enumerate() {
                let wgsl = vec3_const(shader, &format!("{name}_ROW{index}"));
                for column in 0..3 {
                    assert_close(
                        row[column],
                        wgsl[column],
                        &format!("{shader_name} {name}[{index}][{column}]"),
                    );
                }
            }
        }
    }
}
