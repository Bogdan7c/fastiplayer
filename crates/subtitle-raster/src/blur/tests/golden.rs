use super::*;
use crate::blur::gauss_kernels::{LANES, plane_stride};

// ---------------------------------------------------------------------------------------
// Золотые файлы libass 0.17.5
// ---------------------------------------------------------------------------------------

/// Параметры золотого случая (общие с генератором в `c_parity.rs`).
pub(in crate::blur) struct GoldenCase {
    pub(in crate::blur) name: &'static str,
    pub(in crate::blur) r2_x: f64,
    pub(in crate::blur) r2_y: f64,
    pub(in crate::blur) box_passes: i32,
}

/// Случаи: каскад `level 0/2/3`, разные оси и `\be`. Дисперсии — `restore_blur` от
/// `\blur` 2 и 1/5 (значения снимались с libass) и произвольное 200 (level 3).
pub(in crate::blur) const GOLDEN_CASES: [GoldenCase; 4] = [
    GoldenCase {
        name: "glyph_17x9_blur2",
        r2_x: 2.77877941395241,
        r2_y: 2.77877941395241,
        box_passes: 0,
    },
    GoldenCase {
        name: "glyph_40x24_aniso_be3",
        r2_x: 0.7868979340171831,
        r2_y: 18.153994807386983,
        box_passes: 3,
    },
    GoldenCase {
        name: "noise_33x20_level2",
        r2_x: 45.56846228483635,
        r2_y: 45.56846228483635,
        box_passes: 0,
    },
    GoldenCase {
        name: "noise_12x6_level3",
        r2_x: 200.0,
        r2_y: 200.0,
        box_passes: 0,
    },
];

/// Бинарный формат харнесса: `left top width height` (i32), `stride` (i64), буфер.
pub(in crate::blur) fn decode_fixture(bytes: &[u8]) -> CoverageBitmap {
    let int = |at: usize| i32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
    CoverageBitmap {
        left: int(0),
        top: int(4),
        width: int(8),
        height: int(12),
        stride: i64::from_le_bytes(bytes[16..24].try_into().expect("8 bytes")) as usize,
        buffer: bytes[24..].to_vec(),
    }
}

macro_rules! golden_files {
    ($name:literal) => {
        (
            include_bytes!(concat!("../../../tests/fixtures/blur/", $name, ".in.bin")).as_slice(),
            include_bytes!(concat!("../../../tests/fixtures/blur/", $name, ".out.bin")).as_slice(),
        )
    };
}

#[test]
fn blur_matches_golden_files_recorded_from_libass() {
    let files = [
        golden_files!("glyph_17x9_blur2"),
        golden_files!("glyph_40x24_aniso_be3"),
        golden_files!("noise_33x20_level2"),
        golden_files!("noise_12x6_level3"),
    ];
    for (case, (input, expected)) in GOLDEN_CASES.iter().zip(files) {
        let mut bitmap = decode_fixture(input);
        let expected = decode_fixture(expected);
        let strength = BlurStrength {
            gaussian: variance(case.r2_x, case.r2_y),
            box_passes: BoxBlurPasses::clamped(case.box_passes),
        };
        synth_blur(
            &mut BlurScratch::new(),
            &mut bitmap,
            strength,
            BitmapSideLimit::UNLIMITED,
        )
        .expect("blur");
        assert_eq!(
            (
                bitmap.left,
                bitmap.top,
                bitmap.width,
                bitmap.height,
                bitmap.stride
            ),
            (
                expected.left,
                expected.top,
                expected.width,
                expected.height,
                expected.stride
            ),
            "геометрия {}",
            case.name
        );
        assert!(
            bitmap.buffer == expected.buffer,
            "пиксели {} расходятся с libass",
            case.name
        );
    }
}

#[test]
fn plane_stride_is_a_multiple_of_the_lane_width() {
    assert_eq!(plane_stride(1), LANES);
    assert_eq!(plane_stride(16), 16);
    assert_eq!(plane_stride(17), 32);
}
