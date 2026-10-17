//! Допуски эталонов слоя 2 (сессия 09): статическая геометрия — повороты, наклон, `\org`.
//!
//! Правила те же, что у [`super::ass_tolerances`]: геометрия картинок точно (позиция ≤ 1 px,
//! размер ≤ 2 px), доля отличающихся пикселей ≤ 1 % закрашенной libass площади, допуск строки —
//! измерение с запасом ~30 %. Строки привязаны к имени ЭТАЛОНА (для кейса в нескольких
//! моментах — `<case>@t<мс:05>`). Ни одной строки с ослабленным пределом площади нет: кейсы
//! с малой площадью (3D-перспектива уменьшает текст) сняты с шрифтом 72 вместо 56, потому что
//! доля шума растёт обратно размеру текста.
//!
//! Про «максимум 130»: пиксель, где у нас альфа 0, а у libass 1–3 (покрытие округлилось по
//! другую сторону порога). Каналы цвета при нулевой альфе у нас нули, у libass — цвет слоя, и
//! метрика «разница канала» показывает ~128, хотя на экране разница невидима (альфа 1/255).
//! Центры масс картинок совпадают с libass до 0.005 px, то есть систематического сдвига нет:
//! остаток — тот же шум субпиксельных расхождений текста, что и без поворота.

use super::ass_tolerances::{AssTolerance, ONE_PERCENT};

/// Кейсы статических поворотов, наклона и `\org`. Измерения 2026-10-17, libass 0.17.5.
pub(super) const LAYER2_STATIC_CASES: &[AssTolerance] = &[
    // измерено: 14 px из 2147 (0.65 %), максимум 6
    AssTolerance {
        name: "ass_rot_3d_behind_camera",
        differing_pixels: 20,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 2976 (0.54 %), максимум 6
    AssTolerance {
        name: "ass_rot_3d_over_90",
        differing_pixels: 22,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 41 px из 6917 (0.59 %), максимум 128
    AssTolerance {
        name: "ass_rot_an7_frz",
        differing_pixels: 55,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 6902 (0.23 %), максимум 7
    AssTolerance {
        name: "ass_rot_an9_frx",
        differing_pixels: 22,
        max_channel_difference: 11,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 34 px из 11314 (0.30 %), максимум 128
    AssTolerance {
        name: "ass_rot_border_shadow",
        differing_pixels: 46,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 25 px из 7875 (0.32 %), максимум 8
    AssTolerance {
        name: "ass_rot_border_shadow_frx",
        differing_pixels: 34,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 9 px из 15728 (0.06 %), максимум 5
    AssTolerance {
        name: "ass_rot_box",
        differing_pixels: 13,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 9 px из 13145 (0.07 %), максимум 6
    AssTolerance {
        name: "ass_rot_box_3d",
        differing_pixels: 13,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 21 px из 3945 (0.53 %), максимум 22
    AssTolerance {
        name: "ass_rot_fax",
        differing_pixels: 29,
        max_channel_difference: 30,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 29 px из 5325 (0.54 %), максимум 128
    AssTolerance {
        name: "ass_rot_fax_fscx",
        differing_pixels: 39,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 19 px из 3927 (0.48 %), максимум 128
    AssTolerance {
        name: "ass_rot_fay",
        differing_pixels: 26,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 19 px из 2809 (0.68 %), максимум 26
    AssTolerance {
        name: "ass_rot_fay_fscy",
        differing_pixels: 26,
        max_channel_difference: 35,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 30 px из 4351 (0.69 %), максимум 128
    AssTolerance {
        name: "ass_rot_fr_alias",
        differing_pixels: 40,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 26 px из 3213 (0.81 %), максимум 19
    AssTolerance {
        name: "ass_rot_frx_45",
        differing_pixels: 35,
        max_channel_difference: 26,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 30 px из 5204 (0.58 %), максимум 128
    AssTolerance {
        name: "ass_rot_frxyz",
        differing_pixels: 40,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 3493 (0.49 %), максимум 6
    AssTolerance {
        name: "ass_rot_fry_45",
        differing_pixels: 24,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 25 px из 4353 (0.57 %), максимум 128
    AssTolerance {
        name: "ass_rot_frz_180",
        differing_pixels: 34,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 28 px из 4346 (0.64 %), максимум 6
    AssTolerance {
        name: "ass_rot_frz_45",
        differing_pixels: 38,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 34 px из 4353 (0.78 %), максимум 22
    AssTolerance {
        name: "ass_rot_frz_neg30",
        differing_pixels: 46,
        max_channel_difference: 30,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 65 px из 7583 (0.86 %), максимум 128
    AssTolerance {
        name: "ass_rot_fscx_frz",
        differing_pixels: 86,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 3 px из 3601 (0.08 %), максимум 5
    AssTolerance {
        name: "ass_rot_org_first_wins",
        differing_pixels: 5,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 3384 (0.50 %), максимум 5
    AssTolerance {
        name: "ass_rot_org_frz",
        differing_pixels: 24,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 31 px из 9886 (0.31 %), максимум 28
    AssTolerance {
        name: "ass_rot_org_no_collision",
        differing_pixels: 42,
        max_channel_difference: 38,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 34 px из 5240 (0.65 %), максимум 128
    AssTolerance {
        name: "ass_rot_reset",
        differing_pixels: 46,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 24 px из 4124 (0.58 %), максимум 5
    AssTolerance {
        name: "ass_rot_runs",
        differing_pixels: 33,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 50 px из 8976 (0.56 %), максимум 27
    AssTolerance {
        name: "ass_rot_style_angle",
        differing_pixels: 66,
        max_channel_difference: 37,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 56 px из 11503 (0.49 %), максимум 128
    AssTolerance {
        name: "ass_rot_two_lines",
        differing_pixels: 74,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 26 px из 6588 (0.39 %), максимум 18
    AssTolerance {
        name: "ass_rot_unpositioned",
        differing_pixels: 35,
        max_channel_difference: 25,
        area_percent_limit: ONE_PERCENT,
    },
];
