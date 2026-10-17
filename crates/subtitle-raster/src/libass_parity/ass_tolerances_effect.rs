//! Допуски эталонов прокрутки (сессия 10b): эффекты `Banner`, `Scroll up`, `Scroll down`.
//!
//! Правила те же, что у [`super::ass_tolerances`]: число, порядок, тип и цвет картинок и их
//! геометрия совпадают с libass (позиция и размер точно: `pos_delta = 0`, `size_delta = 0` во
//! всех кейсах), доля отличающихся пикселей <= 1 % закрашенной libass площади. Остаток — шум
//! растеризации букв слоя 1 (не зависит от прокрутки), а максимум 128 при нулевой альфе — тот же
//! эффект, что описан в [`super::ass_tolerances_layer2`]. Моменты кейсов подобраны так, чтобы
//! в полосе прокрутки оставалось не меньше половины строки: на узкой полоске шум краёв букв
//! занимает больший процент площади. Строки привязаны к имени эталона `<case>@t<мс:05>`.

use super::ass_tolerances::{AssTolerance, ONE_PERCENT};

/// Кейсы прокрутки. Измерения 2026-10-17, libass 0.17.5.
pub(super) const LAYER3_EFFECT_CASES: &[AssTolerance] = &[
    // измерено: 18 px из 3064 (0.59 %), максимум 4
    // Строки внутри самой широкой строки: `an9` выравнивает вправо, `an2` по центру (`justify = halign; halign = LEFT`).
    AssTolerance {
        name: "ass_fx_banner_an9@t01000",
        differing_pixels: 24,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 51 px из 9474 (0.54 %), максимум 6
    AssTolerance {
        name: "ass_fx_banner_an9@t02500",
        differing_pixels: 68,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 23 px из 3586 (0.64 %), максимум 4
    // Задержка 0 и -5 дают одинаковый пол в 1 мс на пиксель хранения (`max(delay / scale, 1)`).
    AssTolerance {
        name: "ass_fx_banner_delay_floor@t00100",
        differing_pixels: 30,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 30 px из 5687 (0.53 %), максимум 4
    AssTolerance {
        name: "ass_fx_banner_delay_floor@t00300",
        differing_pixels: 40,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 784 (0.51 %), максимум 4
    // Banner слева направо (`Banner;10;1`): правый край блока на `x2scr(shift)`; в первый момент текст частично за левым краем.
    AssTolerance {
        name: "ass_fx_banner_lr@t00500",
        differing_pixels: 6,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 2192 (0.78 %), максимум 4
    AssTolerance {
        name: "ass_fx_banner_lr@t01500",
        differing_pixels: 22,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 2192 (0.78 %), максимум 4
    AssTolerance {
        name: "ass_fx_banner_lr@t03000",
        differing_pixels: 22,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 17 px из 2192 (0.78 %), максимум 4
    AssTolerance {
        name: "ass_fx_banner_lr@t04500",
        differing_pixels: 22,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 5 px из 1579 (0.32 %), максимум 4
    // Banner принудительно выключает перенос: длинная строка остаётся одной и уходит за край кадра.
    AssTolerance {
        name: "ass_fx_banner_nowrap@t01000",
        differing_pixels: 7,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 23 px из 4115 (0.56 %), максимум 6
    AssTolerance {
        name: "ass_fx_banner_nowrap@t02500",
        differing_pixels: 30,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 23 px из 3121 (0.74 %), максимум 128
    // Задержка пересчитывается: PlayRes 320x180 на хранении 640x360, `Banner;3` -> 1 пиксель хранения -> 2 мс на пиксель PlayRes.
    AssTolerance {
        name: "ass_fx_banner_playres@t00200",
        differing_pixels: 30,
        max_channel_difference: 128,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 23 px из 3121 (0.74 %), максимум 128
    AssTolerance {
        name: "ass_fx_banner_playres@t00400",
        differing_pixels: 30,
        max_channel_difference: 128,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 23 px из 3121 (0.74 %), максимум 128
    AssTolerance {
        name: "ass_fx_banner_playres@t00600",
        differing_pixels: 30,
        max_channel_difference: 128,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 5 px из 1836 (0.27 %), максимум 4
    // Banner с `\pos`: x задаёт эффект, y остаётся от `\pos`.
    AssTolerance {
        name: "ass_fx_banner_pos@t01000",
        differing_pixels: 7,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 27 px из 3999 (0.68 %), максимум 5
    AssTolerance {
        name: "ass_fx_banner_pos@t02500",
        differing_pixels: 36,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 229 (0.00 %), максимум 0
    // Banner справа налево, 10 мс на пиксель: позиция блока по `scroll_shift` совпадает с libass до пикселя.
    AssTolerance {
        name: "ass_fx_banner_rl@t00100",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 7 px из 953 (0.73 %), максимум 4
    AssTolerance {
        name: "ass_fx_banner_rl@t00500",
        differing_pixels: 9,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 2206 (0.73 %), максимум 4
    AssTolerance {
        name: "ass_fx_banner_rl@t01500",
        differing_pixels: 21,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 2206 (0.73 %), максимум 4
    AssTolerance {
        name: "ass_fx_banner_rl@t03000",
        differing_pixels: 21,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 46 px из 9891 (0.47 %), максимум 6
    // Мало чисел и неизвестные имена: эффект игнорируется, реплики рисуются как обычные.
    AssTolerance {
        name: "ass_fx_malformed",
        differing_pixels: 61,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 26 px из 3179 (0.82 %), максимум 5
    // `Scroll down;100;300;20`: то же для движения вниз.
    AssTolerance {
        name: "ass_fx_scroll_down@t01200",
        differing_pixels: 34,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 32 px из 4953 (0.65 %), максимум 5
    AssTolerance {
        name: "ass_fx_scroll_down@t02000",
        differing_pixels: 42,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 47 px из 5614 (0.84 %), максимум 6
    AssTolerance {
        name: "ass_fx_scroll_down@t04000",
        differing_pixels: 62,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 15 px из 1765 (0.85 %), максимум 6
    AssTolerance {
        name: "ass_fx_scroll_down@t05400",
        differing_pixels: 20,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 3638 (0.44 %), максимум 4
    // `Scroll up` с `\pos`: y задаёт эффект, x остаётся от `\pos`.
    AssTolerance {
        name: "ass_fx_scroll_pos@t01000",
        differing_pixels: 21,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 3638 (0.44 %), максимум 4
    AssTolerance {
        name: "ass_fx_scroll_pos@t03000",
        differing_pixels: 21,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 10 px из 1263 (0.79 %), максимум 15
    // Полоса пересекает строки прямоугольного `\clip`; x клипа остаётся своим.
    AssTolerance {
        name: "ass_fx_scroll_rect_clip@t00500",
        differing_pixels: 13,
        max_channel_difference: 17,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 5 px из 1305 (0.38 %), максимум 4
    AssTolerance {
        name: "ass_fx_scroll_rect_clip@t05500",
        differing_pixels: 7,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // Полоса пересекается с дырой `\iclip`: дыра шире полосы показывает текст вне полосы (как в libass).
    // измерено: 56 px из 9121 (0.61 %), максимум 6
    AssTolerance {
        name: "ass_fx_scroll_iclip@t00500",
        differing_pixels: 66,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 30 px из 5105 (0.59 %), максимум 6
    AssTolerance {
        name: "ass_fx_scroll_iclip@t02000",
        differing_pixels: 40,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 12 px из 2469 (0.49 %), максимум 6
    AssTolerance {
        name: "ass_fx_scroll_iclip@t04000",
        differing_pixels: 22,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 25 px из 3286 (0.76 %), максимум 6
    // Границы полосы `300;100` упорядочиваются (min/max), как в libass.
    AssTolerance {
        name: "ass_fx_scroll_reversed",
        differing_pixels: 33,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 21 px из 2435 (0.86 %), максимум 6
    // `Scroll up;100;300;20`: текст виден только в полосе 100..300 (клип по `lround(y2scr_pos)`), снизу и сверху.
    AssTolerance {
        name: "ass_fx_scroll_up@t01200",
        differing_pixels: 28,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 47 px из 5614 (0.84 %), максимум 6
    AssTolerance {
        name: "ass_fx_scroll_up@t02400",
        differing_pixels: 62,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 47 px из 5614 (0.84 %), максимум 6
    AssTolerance {
        name: "ass_fx_scroll_up@t04000",
        differing_pixels: 62,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 32 px из 3849 (0.83 %), максимум 5
    AssTolerance {
        name: "ass_fx_scroll_up@t05000",
        differing_pixels: 42,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
];
