use super::*;
use crate::blur::gauss_method::find_best_method;
use crate::blur::params::frexp;

// ---------------------------------------------------------------------------------------
// Параметры (ass_render.c)
// ---------------------------------------------------------------------------------------

#[test]
fn quantize_and_restore_blur_match_libass() {
    // Снято с libass 0.17.5: radius = blur * 2/sqrt(ln 256), затем quantize_blur/restore_blur.
    // (\blur, level, shadow_mask, r2)
    let table: [(f64, i32, i32, f64); 16] = [
        (0.0, 0, 7, 0.0),
        (0.01, 0, 7, 0.0),
        (0.1, 1, 7, 0.015686174466898746),
        (0.25, 2, 7, 0.06299051396074708),
        (0.5, 3, 7, 0.14228427130361235),
        (1.0, 7, 7, 0.7868979340171831),
        (2.0, 13, 7, 2.77877941395241),
        (3.0, 20, 7, 6.7612992219394),
        (5.0, 32, 7, 18.153994807386983),
        (8.0, 49, 7, 45.56846228483635),
        (13.0, 76, 7, 122.33923176505971),
        (20.0, 109, 7, 288.4952570325853),
        (30.0, 150, 7, 649.9229536525589),
        (50.0, 216, 15, 1797.9490350970111),
        (75.0, 280, 15, 4036.6140441272573),
        (100.0, 332, 15, 7233.695807678659),
    ];
    for (blur, level, mask, r2) in table {
        let quantized = quantize_blur(blur * blur_radius_scale());
        assert_eq!(
            quantized,
            QuantizedBlur {
                level,
                shadow_mask: mask
            },
            "blur {blur}"
        );
        let restored = restore_blur(quantized.level);
        assert!(
            (restored - r2).abs() <= r2.abs() * 1e-12,
            "blur {blur}: restored {restored}, libass {r2}"
        );
    }
}

#[test]
fn box_blur_padding_matches_ass_be_padding() {
    // Снято с libass: be 0..=10 и 120, 127.
    let expected = [
        (0, 0),
        (1, 1),
        (2, 2),
        (3, 3),
        (4, 4),
        (5, 4),
        (6, 4),
        (7, 4),
        (8, 5),
    ];
    for (be, padding) in expected {
        assert_eq!(BoxBlurPasses::clamped(be).padding_px(), padding, "be {be}");
    }
    assert_eq!(BoxBlurPasses::clamped(127).padding_px(), 5);
    assert_eq!(
        BoxBlurPasses::clamped(10_000).count(),
        127,
        "верхний зажим как в парсере"
    );
    assert_eq!(
        BoxBlurPasses::clamped(-3),
        BoxBlurPasses::NONE,
        "нижний зажим"
    );
}

#[test]
fn shadow_offset_is_rounded_to_the_blur_mask() {
    // (x + (mask >> 1)) & ~mask — формулы ass_render.c:2521.
    assert_eq!(quantize_shadow_offset(13, 7), 16);
    assert_eq!(quantize_shadow_offset(12, 7), 8);
    assert_eq!(quantize_shadow_offset(13, 15), 16);
    assert_eq!(quantize_shadow_offset(0, 7), 0);
    assert_eq!(quantize_shadow_offset(-5, 7), -8);
    // Нулевое размытие даёт маску 7: смещение тени кратно 8 (1/8 пикселя).
    let no_blur = quantize_blur(0.0);
    assert_eq!(quantize_shadow_offset(64, no_blur.shadow_mask), 64);
}

#[test]
fn frexp_matches_c_semantics() {
    assert_eq!(frexp(1.0), (0.5, 1));
    assert_eq!(frexp(8.0), (0.5, 4));
    assert_eq!(frexp(0.3), (0.6, -1));
    assert_eq!(frexp(-12.0), (-0.75, 4));
    assert_eq!(frexp(0.0), (0.0, 0));
    assert_eq!(
        frexp(f64::from_bits(1)),
        (0.5, -1073),
        "наименьшее денормализованное"
    );
    let (mantissa, exponent) = frexp(f64::INFINITY);
    assert!(mantissa.is_infinite() && exponent == 0);
}

// ---------------------------------------------------------------------------------------
// Выбор метода (ass_blur.c: find_best_method)
// ---------------------------------------------------------------------------------------

#[test]
fn best_method_coefficients_match_libass() {
    // Снято с libass 0.17.5: (r2, level, radius, первые `radius` коэффициентов).
    let table: [(f64, u32, usize, &[i16]); 14] = [
        (0.0, 0, 4, &[0, 0, 0, 0]),
        (0.1, 0, 4, &[3255, 6, 0, 0]),
        (0.3, 0, 4, &[9229, 150, 0, 0]),
        (0.49, 0, 4, &[13435, 655, 0, 0]),
        (0.5, 0, 4, &[13601, 676, 4, 0]),
        (1.0, 0, 4, &[15858, 3538, 290, 9]),
        (2.0, 0, 5, &[14398, 6802, 1949, 339, 36]),
        (4.0, 0, 6, &[11542, 7934, 4249, 1774, 580, 150]),
        (10.0, 1, 4, &[14606, 6567, 1696, 315]),
        (30.0, 1, 8, &[9271, 7462, 5174, 3128, 1600, 757, 247, 139]),
        (100.0, 2, 7, &[10232, 7726, 4948, 2539, 1200, 349, 192]),
        (300.0, 3, 6, &[11655, 7961, 4104, 1731, 472, 177]),
        (1000.0, 4, 5, &[12764, 7680, 3549, 1003, 350]),
        (5000.0, 5, 6, &[11440, 7980, 4211, 1873, 521, 219]),
    ];
    for (r2, level, radius, coeff) in table {
        let method = find_best_method(r2);
        assert_eq!((method.level, method.radius), (level, radius), "r2 {r2}");
        assert_eq!(&method.coeff[..radius], coeff, "r2 {r2}");
        assert!(
            method.coeff[radius..].iter().all(|&c| c == 0),
            "хвост коэффициентов r2 {r2}"
        );
    }
}
