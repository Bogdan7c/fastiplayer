use super::*;

// ---------------------------------------------------------------------------------------
// Box-размытие \be
// ---------------------------------------------------------------------------------------

fn box_blurred(mut bitmap: CoverageBitmap, passes: i32) -> CoverageBitmap {
    box_blur(
        &mut BlurScratch::new(),
        &mut bitmap,
        BoxBlurPasses::clamped(passes),
    )
    .expect("blur");
    bitmap
}

#[test]
fn box_blur_of_an_impulse_gives_the_exact_kernel() {
    // Снято с libass 0.17.5 (ass_synth_blur, be = 1, 2, 3 на импульсе 255 в центре 3x3).
    let kernel = |be| visible_rows(&box_blurred(impulse(3, 3, 1, 1), be));
    assert_eq!(kernel(1), [[15, 31, 15], [31, 63, 31], [15, 31, 15]]);
    assert_eq!(kernel(2), [[16, 24, 16], [24, 36, 24], [16, 24, 16]]);
    assert_eq!(kernel(3), [[12, 17, 12], [17, 25, 17], [12, 17, 12]]);
}

#[test]
fn box_blur_series_spreads_an_impulse_over_a_five_pixel_window() {
    // libass 0.17.5, импульс 255 в центре 5x5.
    let rows = |be| visible_rows(&box_blurred(impulse(5, 5, 2, 2), be));
    assert_eq!(rows(2)[0], [1, 4, 6, 4, 1]);
    assert_eq!(rows(2)[2], [6, 24, 36, 24, 6]);
    assert_eq!(rows(3)[2], [7, 18, 25, 18, 7]);
    assert_eq!(rows(4)[1], [4, 10, 13, 10, 4]);
}

#[test]
fn box_blur_without_passes_keeps_the_bitmap() {
    let original = noise_bitmap(21, 7, 5);
    assert_eq!(box_blurred(original.clone(), 0), original);
}

#[test]
fn box_blur_does_not_touch_bytes_right_of_the_visible_width() {
    let mut bitmap = noise_bitmap(21, 7, 9);
    // Метки в области выравнивания: она принадлежит владельцу битмапа, а не размытию.
    for y in 0..7 {
        for x in 21..bitmap.stride {
            bitmap.buffer[y * bitmap.stride + x] = 0xA5;
        }
    }
    let blurred = box_blurred(bitmap.clone(), 3);
    for y in 0..7 {
        let range = y * bitmap.stride + 21..(y + 1) * bitmap.stride;
        assert_eq!(
            blurred.buffer[range.clone()],
            bitmap.buffer[range],
            "строка {y}"
        );
    }
    assert_ne!(
        visible_rows(&blurred),
        visible_rows(&bitmap),
        "видимая часть размыта"
    );
}

#[test]
fn box_blur_has_defined_result_for_single_row_and_column() {
    // В libass такие размеры — неопределённое поведение (`ASSUME(width > 1 && height > 1)`,
    // харнесс падает): композит всегда добавляет отступ. Здесь результат определён
    // нулевым продолжением: по оси длиной 1 ядро [1 2 1] даёт 2*b, затем >> 4.
    let single_row = box_blurred(bitmap_from_rows(&[&[0, 128, 0]]), 1);
    assert_eq!(visible_rows(&single_row), [[16, 32, 16]]);
    let single_column = box_blurred(bitmap_from_rows(&[&[0], &[128], &[0]]), 1);
    assert_eq!(visible_rows(&single_column), [[16], [32], [16]]);
}

#[test]
fn retained_memory_counts_box_row_buffers_and_release_frees_them() {
    // Box-размытие не трогает плоскости каскада, но держит строки сумм: их тоже обязан
    // учитывать и освобождать лимит удержанной памяти.
    let mut scratch = BlurScratch::new();
    assert_eq!(scratch.retained_bytes(), 0);
    let mut bitmap = noise_bitmap(64, 16, 3);
    box_blur(&mut scratch, &mut bitmap, BoxBlurPasses::clamped(2)).expect("blur");
    let held = scratch.retained_bytes();
    assert!(held >= 64 * size_of::<u16>(), "{held}");
    // Лимит ровно по объёму держит память, на байт меньше - освобождает.
    scratch.release_if_larger_than(held);
    assert_eq!(scratch.retained_bytes(), held);
    scratch.release_if_larger_than(held - 1);
    assert_eq!(scratch.retained_bytes(), 0);
}
