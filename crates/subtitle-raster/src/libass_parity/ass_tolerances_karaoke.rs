//! Допуски эталонов караоке (сессия 10b): `\k`, `\K`, `\kf`, `\ko`, `\kt`, совместимость
//! `\k12345\k0`, смена стиля внутри слова, направление заливки `\kf` при `\frz`, 3D-повороты,
//! клипы, обводка с тенью, перенос на две строки, затухание, прозрачные цвета и `\t`. Каждый
//! кейс снят до начала слога, внутри слога и после него.
//!
//! Правила те же, что у [`super::ass_tolerances`]: число, порядок, тип и цвет картинок совпадают с
//! libass, позиция и размер точно, доля отличающихся пикселей <= 1 % закрашенной libass площади.
//! Остаток не зависит от момента (одно и то же число пикселей в каждом моменте кейса): это шум
//! растеризации букв слоя 1 (пиксель с нулевой альфой у нас и 1-3 у libass; у повёрнутых и
//! обрезанных кейсов отдельные пиксели расходятся сильнее, но тоже одинаково во всех моментах),
//! а не расхождение границы заливки: граница `\kf` совпадает по числу, положению и размеру
//! картинок. Строки привязаны к имени эталона `<case>@t<мс:05>`.

use super::ass_tolerances::{AssTolerance, ONE_PERCENT};

/// Кейсы караоке. Измерения 2026-10-17, libass 0.17.5.
pub(super) const LAYER3_KARAOKE_CASES: &[AssTolerance] = &[
    // измерено: 16 px из 3936 (0.41 %), максимум 16
    AssTolerance {
        name: "ass_k_align@t00300",
        differing_pixels: 22,
        max_channel_difference: 18,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 3936 (0.41 %), максимум 16
    AssTolerance {
        name: "ass_k_align@t00800",
        differing_pixels: 22,
        max_channel_difference: 18,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 3936 (0.41 %), максимум 16
    AssTolerance {
        name: "ass_k_align@t01500",
        differing_pixels: 22,
        max_channel_difference: 18,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 35 px из 6927 (0.51 %), максимум 128
    AssTolerance {
        name: "ass_k_alpha@t00300",
        differing_pixels: 47,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 48 px из 7844 (0.61 %), максимум 255
    AssTolerance {
        name: "ass_k_alpha@t00800",
        differing_pixels: 64,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 48 px из 8226 (0.58 %), максимум 255
    AssTolerance {
        name: "ass_k_alpha@t01500",
        differing_pixels: 64,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 48 px из 8226 (0.58 %), максимум 255
    AssTolerance {
        name: "ass_k_alpha@t02500",
        differing_pixels: 64,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 19 px из 9258 (0.21 %), максимум 128
    AssTolerance {
        name: "ass_k_basic@t00100",
        differing_pixels: 26,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 19 px из 9881 (0.19 %), максимум 128
    AssTolerance {
        name: "ass_k_basic@t00450",
        differing_pixels: 26,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 23 px из 10333 (0.22 %), максимум 128
    AssTolerance {
        name: "ass_k_basic@t00950",
        differing_pixels: 31,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 33 px из 11106 (0.30 %), максимум 128
    AssTolerance {
        name: "ass_k_basic@t01500",
        differing_pixels: 44,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 33 px из 11106 (0.30 %), максимум 128
    AssTolerance {
        name: "ass_k_basic@t03000",
        differing_pixels: 44,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 43 px из 16311 (0.26 %), максимум 128
    AssTolerance {
        name: "ass_k_border_shadow@t00300",
        differing_pixels: 57,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 43 px из 16311 (0.26 %), максимум 128
    AssTolerance {
        name: "ass_k_border_shadow@t00800",
        differing_pixels: 57,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 43 px из 16311 (0.26 %), максимум 128
    AssTolerance {
        name: "ass_k_border_shadow@t01500",
        differing_pixels: 57,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 43 px из 16311 (0.26 %), максимум 128
    AssTolerance {
        name: "ass_k_border_shadow@t02500",
        differing_pixels: 57,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 28 px из 6308 (0.44 %), максимум 128
    AssTolerance {
        name: "ass_k_clip@t00300",
        differing_pixels: 38,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 28 px из 6308 (0.44 %), максимум 128
    AssTolerance {
        name: "ass_k_clip@t00800",
        differing_pixels: 38,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 28 px из 6308 (0.44 %), максимум 128
    AssTolerance {
        name: "ass_k_clip@t01500",
        differing_pixels: 38,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 2856 (0.04 %), максимум 4
    AssTolerance {
        name: "ass_k_compat@t00300",
        differing_pixels: 3,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 2856 (0.04 %), максимум 4
    AssTolerance {
        name: "ass_k_compat@t00800",
        differing_pixels: 3,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 2856 (0.04 %), максимум 4
    AssTolerance {
        name: "ass_k_compat@t01600",
        differing_pixels: 3,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 2856 (0.04 %), максимум 4
    AssTolerance {
        name: "ass_k_compat@t03000",
        differing_pixels: 3,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 2 px из 2032 (0.10 %), максимум 4
    AssTolerance {
        name: "ass_k_default_length@t00500",
        differing_pixels: 4,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 2 px из 2032 (0.10 %), максимум 4
    AssTolerance {
        name: "ass_k_default_length@t01500",
        differing_pixels: 4,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 2 px из 2032 (0.10 %), максимум 4
    AssTolerance {
        name: "ass_k_default_length@t02500",
        differing_pixels: 4,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 7 px из 4833 (0.14 %), максимум 65
    AssTolerance {
        name: "ass_k_fad@t00200",
        differing_pixels: 11,
        max_channel_difference: 67,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 18 px из 4846 (0.37 %), максимум 6
    AssTolerance {
        name: "ass_k_fad@t00700",
        differing_pixels: 25,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 18 px из 4846 (0.37 %), максимум 6
    AssTolerance {
        name: "ass_k_fad@t01200",
        differing_pixels: 25,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 18 px из 4846 (0.37 %), максимум 6
    AssTolerance {
        name: "ass_k_fad@t02200",
        differing_pixels: 25,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 2969 (0.57 %), максимум 5
    AssTolerance {
        name: "ass_k_frx@t00300",
        differing_pixels: 24,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 2969 (0.57 %), максимум 5
    AssTolerance {
        name: "ass_k_frx@t00700",
        differing_pixels: 24,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 2969 (0.57 %), максимум 5
    AssTolerance {
        name: "ass_k_frx@t01200",
        differing_pixels: 24,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 2969 (0.57 %), максимум 5
    AssTolerance {
        name: "ass_k_frx@t02500",
        differing_pixels: 24,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 12 px из 3090 (0.39 %), максимум 255
    AssTolerance {
        name: "ass_k_frz@t00300",
        differing_pixels: 17,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 12 px из 3090 (0.39 %), максимум 255
    AssTolerance {
        name: "ass_k_frz@t00700",
        differing_pixels: 17,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 12 px из 3090 (0.39 %), максимум 255
    AssTolerance {
        name: "ass_k_frz@t01200",
        differing_pixels: 17,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 12 px из 3090 (0.39 %), максимум 255
    AssTolerance {
        name: "ass_k_frz@t02500",
        differing_pixels: 17,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 4814 (0.35 %), максимум 255
    AssTolerance {
        name: "ass_k_iclip@t00300",
        differing_pixels: 24,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 4814 (0.35 %), максимум 255
    AssTolerance {
        name: "ass_k_iclip@t00800",
        differing_pixels: 24,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 4814 (0.35 %), максимум 255
    AssTolerance {
        name: "ass_k_iclip@t01500",
        differing_pixels: 24,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 2789 (0.04 %), максимум 4
    AssTolerance {
        name: "ass_k_kt@t00100",
        differing_pixels: 3,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 2789 (0.04 %), максимум 4
    AssTolerance {
        name: "ass_k_kt@t00400",
        differing_pixels: 3,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 2789 (0.04 %), максимум 4
    AssTolerance {
        name: "ass_k_kt@t00800",
        differing_pixels: 3,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 2789 (0.04 %), максимум 4
    AssTolerance {
        name: "ass_k_kt@t01600",
        differing_pixels: 3,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 2789 (0.04 %), максимум 4
    AssTolerance {
        name: "ass_k_kt@t02500",
        differing_pixels: 3,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 2789 (0.04 %), максимум 4
    AssTolerance {
        name: "ass_k_kt@t04000",
        differing_pixels: 3,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 6 px из 2786 (0.22 %), максимум 6
    AssTolerance {
        name: "ass_k_style_change@t00300",
        differing_pixels: 9,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 6 px из 2786 (0.22 %), максимум 6
    AssTolerance {
        name: "ass_k_style_change@t00700",
        differing_pixels: 9,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 6 px из 2786 (0.22 %), максимум 6
    AssTolerance {
        name: "ass_k_style_change@t01200",
        differing_pixels: 9,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 6 px из 2786 (0.22 %), максимум 6
    AssTolerance {
        name: "ass_k_style_change@t02500",
        differing_pixels: 9,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 9 px из 4260 (0.21 %), максимум 255
    AssTolerance {
        name: "ass_k_t@t00300",
        differing_pixels: 13,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 15 px из 4605 (0.33 %), максимум 255
    AssTolerance {
        name: "ass_k_t@t00800",
        differing_pixels: 21,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 32 px из 5172 (0.62 %), максимум 7
    AssTolerance {
        name: "ass_k_t@t01500",
        differing_pixels: 43,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 14 px из 5698 (0.25 %), максимум 5
    AssTolerance {
        name: "ass_k_t@t02500",
        differing_pixels: 20,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 7 px из 5117 (0.14 %), максимум 7
    AssTolerance {
        name: "ass_k_wrap@t00300",
        differing_pixels: 11,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 7 px из 5117 (0.14 %), максимум 7
    AssTolerance {
        name: "ass_k_wrap@t00900",
        differing_pixels: 11,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 7 px из 5117 (0.14 %), максимум 7
    AssTolerance {
        name: "ass_k_wrap@t01500",
        differing_pixels: 11,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 7 px из 5117 (0.14 %), максимум 7
    AssTolerance {
        name: "ass_k_wrap@t02500",
        differing_pixels: 11,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 7 px из 5117 (0.14 %), максимум 7
    AssTolerance {
        name: "ass_k_wrap@t03500",
        differing_pixels: 11,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
];
