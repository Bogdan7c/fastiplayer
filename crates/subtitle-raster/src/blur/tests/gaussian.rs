use super::*;
use crate::blur::gauss_method::find_best_method;

// ---------------------------------------------------------------------------------------
// Гауссово размытие
// ---------------------------------------------------------------------------------------

#[test]
fn gaussian_blur_enlarges_the_bitmap_like_libass() {
    // r2 = 1: level 0, radius 4: слева/сверху +4, ширина/высота +8.
    let mut bitmap = impulse(17, 9, 8, 4);
    bitmap.left = 10;
    bitmap.top = -3;
    gaussian(&mut bitmap, 1.0, 1.0).expect("blur");
    assert_eq!((bitmap.left, bitmap.top), (6, -7));
    assert_eq!((bitmap.width, bitmap.height, bitmap.stride), (25, 17, 32));
    assert_eq!(bitmap.buffer.len(), 32 * 17);
    assert!(padding_is_zero(&bitmap));
}

#[test]
fn gaussian_blur_uses_independent_axes() {
    // X: r2 = 1 (radius 4, level 0); Y: r2 = 4 (radius 6, level 0).
    let mut bitmap = impulse(16, 16, 8, 8);
    gaussian(&mut bitmap, 1.0, 4.0).expect("blur");
    assert_eq!((bitmap.left, bitmap.top), (-4, -6));
    assert_eq!((bitmap.width, bitmap.height), (16 + 8, 16 + 12));
}

#[test]
fn gaussian_blur_of_an_impulse_is_centered_symmetric_and_keeps_energy() {
    let mut bitmap = impulse(15, 15, 7, 7);
    gaussian(&mut bitmap, 4.0, 4.0).expect("blur");
    let rows = visible_rows(&bitmap);
    let (cx, cy) = (7 + (-bitmap.left) as usize, 7 + (-bitmap.top) as usize);
    let peak = rows.iter().flatten().copied().max().expect("non-empty");
    assert_eq!(rows[cy][cx], peak, "максимум в исходной точке");
    assert!((5..=30).contains(&peak), "пик {peak}");
    // Дрожание упаковки (±0.5 уровня) допускает расхождение симметричных пикселей на 1.
    for offset in 1..=5usize {
        for (a, b) in [
            (rows[cy][cx - offset], rows[cy][cx + offset]),
            (rows[cy - offset][cx], rows[cy + offset][cx]),
        ] {
            assert!(
                a.abs_diff(b) <= 1,
                "симметрия на расстоянии {offset}: {a} против {b}"
            );
        }
    }
    let energy: u32 = rows.iter().flatten().map(|&v| u32::from(v)).sum();
    assert!(
        energy.abs_diff(255) <= 40,
        "сумма покрытия {energy} далека от 255"
    );
}

#[test]
fn gaussian_blur_keeps_a_flat_field_flat_in_the_middle() {
    let mut bitmap = CoverageBitmap::zeroed(96, 96).expect("size");
    bitmap.buffer.fill(255);
    gaussian(&mut bitmap, 9.0, 9.0).expect("blur");
    let rows = visible_rows(&bitmap);
    let (cx, cy) = ((-bitmap.left) as usize + 48, (-bitmap.top) as usize + 48);
    assert!(
        rows[cy][cx] >= 254,
        "центр плоского поля потерял яркость: {}",
        rows[cy][cx]
    );
    assert_eq!(rows[0][0], 0, "угол далеко от изображения");
}

#[test]
fn gaussian_blur_result_does_not_depend_on_scratch_history() {
    let big = noise_bitmap(200, 90, 3);
    let small = noise_bitmap(19, 11, 4);
    let params = variance(40.0, 3.0);

    let mut fresh_scratch = BlurScratch::new();
    let mut expected = small.clone();
    gaussian_blur(
        &mut fresh_scratch,
        &mut expected,
        params,
        BitmapSideLimit::UNLIMITED,
    )
    .expect("blur");

    let mut used_scratch = BlurScratch::new();
    let mut warmup = big;
    gaussian_blur(
        &mut used_scratch,
        &mut warmup,
        variance(300.0, 300.0),
        BitmapSideLimit::UNLIMITED,
    )
    .expect("blur");
    let mut actual = small;
    gaussian_blur(
        &mut used_scratch,
        &mut actual,
        params,
        BitmapSideLimit::UNLIMITED,
    )
    .expect("blur");
    assert_eq!(
        actual, expected,
        "остатки прошлых буферов повлияли на результат"
    );

    used_scratch.release_memory();
    assert!(used_scratch.front.is_empty() && used_scratch.back.is_empty());
}

#[test]
fn gaussian_blur_failures_leave_the_bitmap_untouched() {
    let original = noise_bitmap(20, 10, 8);
    let checks = [
        (variance(f64::NAN, 1.0), BitmapSideLimit::UNLIMITED),
        (variance(1.0, -0.5), BitmapSideLimit::UNLIMITED),
        (variance(f64::INFINITY, 1.0), BitmapSideLimit::UNLIMITED),
    ];
    for (params, limit) in checks {
        let mut bitmap = original.clone();
        let error = gaussian_blur(&mut BlurScratch::new(), &mut bitmap, params, limit)
            .expect_err("invalid variance");
        assert!(
            matches!(error, BlurError::InvalidVariance { .. }),
            "{error}"
        );
        assert_eq!(bitmap, original);
    }

    // Лимит стороны: результат 28x18 не помещается в 20.
    let mut bitmap = original.clone();
    let error = gaussian_blur(
        &mut BlurScratch::new(),
        &mut bitmap,
        variance(1.0, 1.0),
        BitmapSideLimit(20),
    )
    .expect_err("side limit");
    assert_eq!(
        error,
        BlurError::ExceedsSideLimit {
            width: 28,
            height: 18,
            limit: 20
        }
    );
    assert_eq!(bitmap, original);

    // Гигантская дисперсия: каскад был бы в сотни миллионов пикселей.
    for huge in [1e12, 1e300] {
        let mut bitmap = original.clone();
        let error = gaussian(&mut bitmap, huge, huge).expect_err("too large");
        assert!(matches!(error, BlurError::TooLarge { .. }), "{error}");
        assert_eq!(bitmap, original);
    }

    // Размеры противоречат буферу.
    let mut broken = original.clone();
    broken.buffer.truncate(broken.stride * 5);
    assert_eq!(
        gaussian(&mut broken, 1.0, 1.0),
        Err(BlurError::InconsistentGeometry)
    );
    let mut narrow_stride = original;
    narrow_stride.stride = 8;
    assert_eq!(
        box_blur(
            &mut BlurScratch::new(),
            &mut narrow_stride,
            BoxBlurPasses::clamped(1)
        ),
        Err(BlurError::InconsistentGeometry)
    );
}

#[test]
fn empty_bitmaps_are_left_alone() {
    let mut empty = CoverageBitmap::zeroed(0, 0).expect("size");
    empty.left = 5;
    let strength = BlurStrength {
        gaussian: variance(9.0, 9.0),
        box_passes: BoxBlurPasses::clamped(3),
    };
    synth_blur(
        &mut BlurScratch::new(),
        &mut empty,
        strength,
        BitmapSideLimit::UNLIMITED,
    )
    .expect("empty is a no-op");
    assert_eq!(
        (empty.left, empty.width, empty.height, empty.buffer.len()),
        (5, 0, 0, 0)
    );
}

// ---------------------------------------------------------------------------------------
// synth_blur: порядок и порог
// ---------------------------------------------------------------------------------------

#[test]
fn synth_blur_applies_gaussian_first_then_box() {
    let original = noise_bitmap(23, 12, 6);
    let strength = BlurStrength {
        gaussian: variance(2.5, 2.5),
        box_passes: BoxBlurPasses::clamped(2),
    };

    let mut combined = original.clone();
    synth_blur(
        &mut BlurScratch::new(),
        &mut combined,
        strength,
        BitmapSideLimit::UNLIMITED,
    )
    .expect("blur");

    let mut stepwise = original;
    let mut scratch = BlurScratch::new();
    gaussian_blur(
        &mut scratch,
        &mut stepwise,
        strength.gaussian,
        BitmapSideLimit::UNLIMITED,
    )
    .expect("gaussian");
    box_blur(&mut scratch, &mut stepwise, strength.box_passes).expect("box");
    assert_eq!(combined, stepwise);
    let radius = find_best_method(2.5).radius as i32;
    assert_eq!(combined.width, 23 + 2 * radius, "гауссиан увеличил битмап");
}

#[test]
fn synth_blur_still_applies_box_passes_after_a_skipped_gaussian() {
    // libass: `ass_synth_blur` не проверяет результат `ass_gaussian_blur`, поэтому при
    // отказе гауссиана (здесь: результат шире предела стороны) `\be` всё равно работает.
    let original = noise_bitmap(23, 12, 6);
    let strength = BlurStrength {
        gaussian: variance(2.5, 2.5),
        box_passes: BoxBlurPasses::clamped(2),
    };
    let mut bitmap = original.clone();
    let outcome = synth_blur(
        &mut BlurScratch::new(),
        &mut bitmap,
        strength,
        BitmapSideLimit(24),
    );
    assert!(
        matches!(outcome, Err(BlurError::ExceedsSideLimit { .. })),
        "пропуск гауссиана должен быть виден вызывающему: {outcome:?}"
    );

    let mut box_only = original.clone();
    box_blur(&mut BlurScratch::new(), &mut box_only, strength.box_passes).expect("box");
    assert_eq!(
        bitmap, box_only,
        "box-проходы применены к исходной картинке"
    );
    assert_ne!(bitmap, original, "box-проходы реально изменили картинку");
    assert_eq!(
        (bitmap.left, bitmap.width),
        (original.left, original.width),
        "гауссиан не увеличивал битмап"
    );
}

#[test]
fn synth_blur_skips_gaussian_below_the_libass_threshold() {
    let original = noise_bitmap(23, 12, 6);
    let mut bitmap = original.clone();
    let strength = BlurStrength {
        gaussian: variance(0.001, 0.0005),
        box_passes: BoxBlurPasses::NONE,
    };
    synth_blur(
        &mut BlurScratch::new(),
        &mut bitmap,
        strength,
        BitmapSideLimit::UNLIMITED,
    )
    .expect("no-op");
    assert_eq!(bitmap, original, "порог ass_synth_blur: > 0.001");

    // Достаточно, чтобы одна ось превысила порог: размывается по обеим.
    let strength = BlurStrength {
        gaussian: variance(0.0, 0.002),
        box_passes: BoxBlurPasses::NONE,
    };
    synth_blur(
        &mut BlurScratch::new(),
        &mut bitmap,
        strength,
        BitmapSideLimit::UNLIMITED,
    )
    .expect("blur");
    assert_eq!((bitmap.width, bitmap.height), (23 + 8, 12 + 8));
}
