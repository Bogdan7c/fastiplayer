//! Допуски эталонов анимаций слоя 2 (сессия 09, этап 2c): `\move`, `\fad`, `\fade` и их
//! сочетания с поворотами, снятые в нескольких моментах времени.
//!
//! Правила те же, что у [`super::ass_tolerances`]: геометрия картинок точно (позиция ≤ 1 px,
//! размер ≤ 2 px), доля отличающихся пикселей ≤ 1 % закрашенной libass площади, допуск строки —
//! измерение с запасом ~30 %. Строки привязаны к имени ЭТАЛОНА `<case>@t<мс:05>`.
//!
//! Ослабленных пределов нет: все измерения укладываются в 0.5 % площади, геометрия совпала
//! точно (`pos_delta = size_delta = 0`) в каждом моменте. Строка с «максимумом 130» — пиксель,
//! где у нас альфа 0, а у libass 1–3 (см. описание в [`super::ass_tolerances_layer2`]).
//! Затухание в середине строки: libass не режет участки по затуханию и берёт его у первого символа участка, поэтому `ass_anim_fad_midline` (одинаковые цвета) не гаснет совсем, а `ass_anim_fad_midline_color` (смена цвета рвёт участок) гаснет только хвост; порт повторяет обе картины.
//! Эталон `ass_anim_fade_7args@t00100` пуст у обоих рендереров: затухание 255 в начале
//! `\fade(255,0,128,...)` полностью прозрачно, и libass не отдаёт картинок.

use super::ass_tolerances::{AssTolerance, ONE_PERCENT};

/// Кейсы анимаций. Измерения 2026-10-17, libass 0.17.5.
pub(super) const LAYER2_ANIMATION_CASES: &[AssTolerance] = &[
    // измерено: 10 px из 5947 (0.17 %), максимум 37
    AssTolerance {
        name: "ass_anim_fad@t00100",
        differing_pixels: 14,
        max_channel_difference: 49,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 6 px из 5954 (0.10 %), максимум 27
    AssTolerance {
        name: "ass_anim_fad@t00250",
        differing_pixels: 10,
        max_channel_difference: 36,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 15 px из 5954 (0.25 %), максимум 6
    AssTolerance {
        name: "ass_anim_fad@t01500",
        differing_pixels: 21,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 5 px из 5951 (0.08 %), максимум 30
    AssTolerance {
        name: "ass_anim_fad@t02800",
        differing_pixels: 9,
        max_channel_difference: 39,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 16107 (0.02 %), максимум 3
    AssTolerance {
        name: "ass_anim_fad_box@t00250",
        differing_pixels: 8,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 16107 (0.05 %), максимум 4
    AssTolerance {
        name: "ass_anim_fad_box@t01500",
        differing_pixels: 12,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 16107 (0.02 %), максимум 3
    AssTolerance {
        name: "ass_anim_fad_box@t02750",
        differing_pixels: 8,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 9374 (0.04 %), максимум 8
    AssTolerance {
        name: "ass_anim_fad_semitransparent@t00250",
        differing_pixels: 8,
        max_channel_difference: 11,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 7 px из 9374 (0.07 %), максимум 16
    AssTolerance {
        name: "ass_anim_fad_semitransparent@t01500",
        differing_pixels: 11,
        max_channel_difference: 21,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 0 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_anim_fade_7args@t00100",
        differing_pixels: 4,
        max_channel_difference: 3,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 4635 (0.17 %), максимум 9
    AssTolerance {
        name: "ass_anim_fade_7args@t00450",
        differing_pixels: 12,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 20 px из 4635 (0.43 %), максимум 18
    AssTolerance {
        name: "ass_anim_fade_7args@t01500",
        differing_pixels: 27,
        max_channel_difference: 24,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 15 px из 4635 (0.32 %), максимум 14
    AssTolerance {
        name: "ass_anim_fade_7args@t02300",
        differing_pixels: 21,
        max_channel_difference: 19,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 7 px из 4631 (0.15 %), максимум 9
    AssTolerance {
        name: "ass_anim_fade_7args@t02900",
        differing_pixels: 11,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 5840 (0.14 %), максимум 4
    AssTolerance {
        name: "ass_anim_fry_fad@t00250",
        differing_pixels: 12,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 31 px из 5840 (0.53 %), максимум 4
    AssTolerance {
        name: "ass_anim_fry_fad@t01500",
        differing_pixels: 42,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 5840 (0.14 %), максимум 4
    AssTolerance {
        name: "ass_anim_fry_fad@t02750",
        differing_pixels: 12,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 3473 (0.12 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_4args@t00000",
        differing_pixels: 8,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 3473 (0.12 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_4args@t01000",
        differing_pixels: 8,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 3473 (0.12 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_4args@t01990",
        differing_pixels: 8,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 3473 (0.12 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_6args@t00250",
        differing_pixels: 8,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 3473 (0.12 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_6args@t01000",
        differing_pixels: 8,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 3473 (0.12 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_6args@t02000",
        differing_pixels: 8,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 24 px из 7347 (0.33 %), максимум 22
    AssTolerance {
        name: "ass_anim_move_an7@t00500",
        differing_pixels: 33,
        max_channel_difference: 29,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 24 px из 7347 (0.33 %), максимум 22
    AssTolerance {
        name: "ass_anim_move_an7@t01000",
        differing_pixels: 33,
        max_channel_difference: 29,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 12 px из 6253 (0.19 %), максимум 53
    AssTolerance {
        name: "ass_anim_move_fad@t00250",
        differing_pixels: 17,
        max_channel_difference: 69,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 25 px из 6248 (0.40 %), максимум 17
    AssTolerance {
        name: "ass_anim_move_fad@t01500",
        differing_pixels: 34,
        max_channel_difference: 23,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 6273 (0.26 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_fad@t02750",
        differing_pixels: 22,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 28 px из 6309 (0.44 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_frz_noorg@t00000",
        differing_pixels: 38,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 28 px из 6309 (0.44 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_frz_noorg@t01000",
        differing_pixels: 38,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 22 px из 6302 (0.35 %), максимум 17
    AssTolerance {
        name: "ass_anim_move_frz_noorg@t01990",
        differing_pixels: 30,
        max_channel_difference: 23,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 32 px из 6306 (0.51 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_frz_org@t00000",
        differing_pixels: 44,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 28 px из 6309 (0.44 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_frz_org@t01000",
        differing_pixels: 38,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 31 px из 6320 (0.49 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_frz_org@t01990",
        differing_pixels: 42,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 3473 (0.12 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_swapped@t00250",
        differing_pixels: 8,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 3473 (0.12 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_swapped@t01000",
        differing_pixels: 8,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 3473 (0.12 %), максимум 128
    AssTolerance {
        name: "ass_anim_move_swapped@t02000",
        differing_pixels: 8,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 6165 (0.13 %), максимум 9
    AssTolerance {
        name: "ass_anim_fad_midline@t00250",
        differing_pixels: 12,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 6165 (0.13 %), максимум 9
    AssTolerance {
        name: "ass_anim_fad_midline@t01500",
        differing_pixels: 12,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 6165 (0.13 %), максимум 9
    AssTolerance {
        name: "ass_anim_fad_midline@t02750",
        differing_pixels: 12,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 6 px из 6165 (0.10 %), максимум 9
    AssTolerance {
        name: "ass_anim_fad_midline_color@t00250",
        differing_pixels: 10,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 6165 (0.13 %), максимум 9
    AssTolerance {
        name: "ass_anim_fad_midline_color@t01500",
        differing_pixels: 12,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 6 px из 6165 (0.10 %), максимум 9
    AssTolerance {
        name: "ass_anim_fad_midline_color@t02750",
        differing_pixels: 10,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
];
