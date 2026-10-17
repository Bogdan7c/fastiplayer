//! Допуски эталонов `\t` (сессия 10b): анимация размеров, обводки и тени, поворотов, цветов,
//! прозрачности, размытия и прямоугольных клипов, а также формы тега (без времён, ускорение,
//! `t2 == 0`, вложенные `\t`, неанимируемые теги внутри, первая `)`), снятая в нескольких
//! моментах: до начала, внутри и после `\t`.
//!
//! Правила те же, что у [`super::ass_tolerances`]: число, порядок, тип и цвет картинок
//! совпадают с libass, позиция и размер точно (в каждом моменте `pos_delta = size_delta = 0`),
//! доля отличающихся пикселей <= 1 % закрашенной libass площади. Остаток — шум растеризации и
//! шейпинга букв слоя 1; максимум 128 — пиксель с нулевой альфой у нас и 1-3 у libass
//! (см. [`super::ass_tolerances_layer2`]). Строки привязаны к имени эталона
//! `<case>@t<мс:05>`.
//!
//! Кейс `ass_t_border_shadow` содержит событие с отрицательной целью (`\bord-9\shad-9`):
//! libass смешивает старое значение с НЕзажатым новым и зажимает результат, поэтому парсер
//! хранит числа как написано, а зажатие делает `RenderState`.

use super::ass_tolerances::{AssTolerance, ONE_PERCENT};

/// Кейсы `\t`. Измерения 2026-10-17, libass 0.17.5.
pub(super) const LAYER3_TRANSFORM_CASES: &[AssTolerance] = &[
    // измерено: 40 px из 8518 (0.47 %), максимум 5
    AssTolerance {
        name: "ass_t_accel@t00000",
        differing_pixels: 52,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 161 px из 22965 (0.70 %), максимум 128
    AssTolerance {
        name: "ass_t_accel@t00500",
        differing_pixels: 210,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 123 px из 18694 (0.66 %), максимум 128
    AssTolerance {
        name: "ass_t_accel@t00800",
        differing_pixels: 160,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 123 px из 18499 (0.66 %), максимум 128
    AssTolerance {
        name: "ass_t_accel@t01500",
        differing_pixels: 160,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 38 px из 9357 (0.41 %), максимум 128
    AssTolerance {
        name: "ass_t_alpha@t00000",
        differing_pixels: 50,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 40 px из 11595 (0.34 %), максимум 128
    AssTolerance {
        name: "ass_t_alpha@t00500",
        differing_pixels: 52,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 41 px из 11588 (0.35 %), максимум 128
    AssTolerance {
        name: "ass_t_alpha@t00800",
        differing_pixels: 54,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 38 px из 11577 (0.33 %), максимум 128
    AssTolerance {
        name: "ass_t_alpha@t01500",
        differing_pixels: 50,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 4310 (0.19 %), максимум 14
    AssTolerance {
        name: "ass_t_accel_edge@t00460",
        differing_pixels: 20,
        max_channel_difference: 16,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 73 px из 9536 (0.77 %), максимум 128
    AssTolerance {
        name: "ass_t_accel_edge@t00500",
        differing_pixels: 85,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 56 px из 9520 (0.59 %), максимум 128
    AssTolerance {
        name: "ass_t_accel_edge@t00540",
        differing_pixels: 68,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 59 px из 9536 (0.62 %), максимум 128
    AssTolerance {
        name: "ass_t_accel_edge@t01000",
        differing_pixels: 71,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 24 px из 10333 (0.23 %), максимум 12
    AssTolerance {
        name: "ass_t_alpha_range@t00000",
        differing_pixels: 34,
        max_channel_difference: 14,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 18 px из 7066 (0.25 %), максимум 12
    AssTolerance {
        name: "ass_t_alpha_range@t00500",
        differing_pixels: 28,
        max_channel_difference: 14,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 21 px из 8773 (0.24 %), максимум 12
    AssTolerance {
        name: "ass_t_alpha_range@t01500",
        differing_pixels: 31,
        max_channel_difference: 14,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 6788 (0.12 %), максимум 6
    AssTolerance {
        name: "ass_t_blur_be@t00000",
        differing_pixels: 12,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 8814 (0.09 %), максимум 6
    AssTolerance {
        name: "ass_t_blur_be@t00500",
        differing_pixels: 12,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 10151 (0.08 %), максимум 6
    AssTolerance {
        name: "ass_t_blur_be@t00800",
        differing_pixels: 12,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 7 px из 10975 (0.06 %), максимум 7
    AssTolerance {
        name: "ass_t_blur_be@t01500",
        differing_pixels: 11,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 53 px из 10140 (0.52 %), максимум 19
    AssTolerance {
        name: "ass_t_border_shadow@t00000",
        differing_pixels: 69,
        max_channel_difference: 27,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 59 px из 9531 (0.62 %), максимум 19
    AssTolerance {
        name: "ass_t_border_shadow@t00500",
        differing_pixels: 77,
        max_channel_difference: 27,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 48 px из 10822 (0.44 %), максимум 255
    AssTolerance {
        name: "ass_t_border_shadow@t00800",
        differing_pixels: 63,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 51 px из 11534 (0.44 %), максимум 255
    AssTolerance {
        name: "ass_t_border_shadow@t01500",
        differing_pixels: 67,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 80 px из 10699 (0.75 %), максимум 128
    AssTolerance {
        name: "ass_t_clip@t00000",
        differing_pixels: 104,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 92 px из 10790 (0.85 %), максимум 128
    AssTolerance {
        name: "ass_t_clip@t00500",
        differing_pixels: 120,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 82 px из 8394 (0.98 %), максимум 128
    AssTolerance {
        name: "ass_t_clip@t00800",
        differing_pixels: 107,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 81 px из 8290 (0.98 %), максимум 128
    AssTolerance {
        name: "ass_t_clip@t01500",
        differing_pixels: 106,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 33 px из 9841 (0.34 %), максимум 128
    AssTolerance {
        name: "ass_t_colors@t00000",
        differing_pixels: 43,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 27 px из 9841 (0.27 %), максимум 128
    AssTolerance {
        name: "ass_t_colors@t00500",
        differing_pixels: 36,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 26 px из 9841 (0.26 %), максимум 128
    AssTolerance {
        name: "ass_t_colors@t00800",
        differing_pixels: 34,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 26 px из 9841 (0.26 %), максимум 128
    AssTolerance {
        name: "ass_t_colors@t01500",
        differing_pixels: 34,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 9 px из 7610 (0.12 %), максимум 4
    AssTolerance {
        name: "ass_t_forms@t00000",
        differing_pixels: 13,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 28 px из 9204 (0.30 %), максимум 22
    AssTolerance {
        name: "ass_t_forms@t01200",
        differing_pixels: 37,
        max_channel_difference: 31,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 87 px из 21053 (0.41 %), максимум 128
    AssTolerance {
        name: "ass_t_forms@t02500",
        differing_pixels: 114,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 148 px из 26313 (0.56 %), максимум 128
    AssTolerance {
        name: "ass_t_forms@t04800",
        differing_pixels: 193,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 690 (0.14 %), максимум 4
    AssTolerance {
        name: "ass_t_mixed_clip@t00000",
        differing_pixels: 5,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 25 px из 2804 (0.89 %), максимум 128
    AssTolerance {
        name: "ass_t_mixed_clip@t00500",
        differing_pixels: 33,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 29 px из 4167 (0.70 %), максимум 128
    AssTolerance {
        name: "ass_t_mixed_clip@t00800",
        differing_pixels: 38,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 30 px из 5232 (0.57 %), максимум 5
    AssTolerance {
        name: "ass_t_mixed_clip@t01500",
        differing_pixels: 39,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 5 px из 5024 (0.10 %), максимум 3
    AssTolerance {
        name: "ass_t_nested@t00000",
        differing_pixels: 9,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 10041 (0.16 %), максимум 6
    AssTolerance {
        name: "ass_t_nested@t00500",
        differing_pixels: 21,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 45 px из 13586 (0.33 %), максимум 16
    AssTolerance {
        name: "ass_t_nested@t00800",
        differing_pixels: 59,
        max_channel_difference: 23,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 61 px из 16800 (0.36 %), максимум 128
    AssTolerance {
        name: "ass_t_nested@t01500",
        differing_pixels: 80,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 35 px из 8083 (0.43 %), максимум 6
    AssTolerance {
        name: "ass_t_non_animatable@t00000",
        differing_pixels: 46,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 35 px из 8083 (0.43 %), максимум 6
    AssTolerance {
        name: "ass_t_non_animatable@t00500",
        differing_pixels: 46,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 37 px из 8974 (0.41 %), максимум 6
    AssTolerance {
        name: "ass_t_non_animatable@t00800",
        differing_pixels: 49,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 36 px из 9723 (0.37 %), максимум 6
    AssTolerance {
        name: "ass_t_non_animatable@t01500",
        differing_pixels: 47,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 34 px из 9635 (0.35 %), максимум 128
    AssTolerance {
        name: "ass_t_quirks@t00000",
        differing_pixels: 45,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 37 px из 10666 (0.35 %), максимум 128
    AssTolerance {
        name: "ass_t_quirks@t00500",
        differing_pixels: 49,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 41 px из 11841 (0.35 %), максимум 128
    AssTolerance {
        name: "ass_t_quirks@t00800",
        differing_pixels: 54,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 53 px из 12700 (0.42 %), максимум 128
    AssTolerance {
        name: "ass_t_quirks@t01500",
        differing_pixels: 69,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 15 px из 6744 (0.22 %), максимум 6
    AssTolerance {
        name: "ass_t_rotation@t00000",
        differing_pixels: 20,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 19 px из 6687 (0.28 %), максимум 9
    AssTolerance {
        name: "ass_t_rotation@t00500",
        differing_pixels: 25,
        max_channel_difference: 14,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 21 px из 6439 (0.33 %), максимум 128
    AssTolerance {
        name: "ass_t_rotation@t00800",
        differing_pixels: 28,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 31 px из 6189 (0.50 %), максимум 13
    AssTolerance {
        name: "ass_t_rotation@t01500",
        differing_pixels: 41,
        max_channel_difference: 19,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 56 px из 13613 (0.41 %), максимум 23
    AssTolerance {
        name: "ass_t_size@t00000",
        differing_pixels: 73,
        max_channel_difference: 32,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 51 px из 12994 (0.39 %), максимум 18
    AssTolerance {
        name: "ass_t_size@t00500",
        differing_pixels: 67,
        max_channel_difference: 26,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 60 px из 13004 (0.46 %), максимум 23
    AssTolerance {
        name: "ass_t_size@t00800",
        differing_pixels: 78,
        max_channel_difference: 32,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 82 px из 16294 (0.50 %), максимум 128
    AssTolerance {
        name: "ass_t_size@t01500",
        differing_pixels: 107,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
];
