//! Допуски эталонов слоя 3 (сессия 10a): векторные рисунки `\p`, `\pbo`, размытие `\blur`, `\be`.
//!
//! Правила те же, что у [`super::ass_tolerances`]: число, порядок, тип и цвет картинок и их
//! геометрия совпадают с libass (позиция <= 1 px, размер <= 2 px), доля отличающихся пикселей
//! <= 1 % закрашенной libass площади. Кейсы из одних рисунков (без букв) совпадают с libass
//! БИТ В БИТ: контур, матрица и растеризатор — тот же порт, поэтому их строки нулевые.
//! Остаток в кейсах с буквами (`inline`, `invalid`, `pbo_*`, `two_lines`, `wrap`,
//! `layers_text`) — обычный шум шейпинга/растеризации букв из слоя 1, к рисункам он не
//! относится (рисунки в тех же кейсах совпадают точно). Максимум 128 при нулевой альфе —
//! тот же эффект, что описан в [`super::ass_tolerances_layer2`].

use super::ass_tolerances::{AssTolerance, ONE_PERCENT};

/// Кейсы рисунков `\p`. Измерения 2026-10-17, libass 0.17.5.
pub(super) const LAYER3_DRAWING_CASES: &[AssTolerance] = &[
    // измерено: 0 px из 14120 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_draw_across_reset",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 7392 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_draw_alignments",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 13270 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_draw_bezier",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 23287 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_draw_border_shadow",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 12816 (0.00 %), максимум 0
    // Стиль `Fontsize: 0`: рисунки рисуются (решение владельца); кейс без букв, потому что
    // libass рисует нулевые буквы размером FreeType-по-умолчанию, а мы их не рисуем.
    AssTolerance {
        name: "ass_draw_fontsize_zero",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 10184 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_draw_escapes",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 12107 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_draw_frz_fscx",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 20820 (0.00 %), максимум 0
    // Координаты `1e999`, `1e30` (-> 0) и 67108964 (заворот по модулю 2^32 -> 100 px) дают те же
    // три рисунка, что и в libass: бит в бит.
    AssTolerance {
        name: "ass_draw_huge_coordinates",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 13 px из 3866 (0.34 %), максимум 4
    AssTolerance {
        name: "ass_draw_inline",
        differing_pixels: 18,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 14 px из 5621 (0.25 %), максимум 128
    AssTolerance {
        name: "ass_draw_invalid",
        differing_pixels: 19,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 10 px из 37076 (0.03 %), максимум 4
    AssTolerance {
        name: "ass_draw_layers_text",
        differing_pixels: 14,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 13258 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_draw_lines_moves",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 4 px из 3447 (0.12 %), максимум 4
    AssTolerance {
        name: "ass_draw_pbo_negative",
        differing_pixels: 6,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 6894 (0.12 %), максимум 4
    AssTolerance {
        name: "ass_draw_pbo_positive",
        differing_pixels: 11,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 19133 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_draw_scale_p2_p4",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 15490 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_draw_spline_close",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 11336 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_draw_spline_extend",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 21 px из 8394 (0.25 %), максимум 4
    AssTolerance {
        name: "ass_draw_two_lines",
        differing_pixels: 28,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 39 px из 11225 (0.35 %), максимум 128
    AssTolerance {
        name: "ass_draw_wrap",
        differing_pixels: 52,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
];

/// Кейсы размытия `\blur`, `\be`. Измерения 2026-10-17, libass 0.17.5.
///
/// Размытие побитово совпадает с C-версией libass (ядра проверены отдельно), поэтому кейсы
/// из одних рисунков и без обводки совпадают точно, а остаток в кейсах с буквами — шум
/// растеризации самих букв слоя 1 (размытие его слегка размазывает). Максимум 128 при
/// нулевой альфе — тот же эффект, что в [`super::ass_tolerances_layer2`].
pub(super) const LAYER3_BLUR_CASES: &[AssTolerance] = &[
    // измерено: 3 px из 12978, максимум 128
    AssTolerance {
        name: "ass_blur_alpha",
        differing_pixels: 6,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 3 px из 7070, максимум 3
    AssTolerance {
        name: "ass_blur_be",
        differing_pixels: 6,
        max_channel_difference: 5,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 1 px из 20719, максимум 3
    AssTolerance {
        name: "ass_blur_box_style",
        differing_pixels: 3,
        max_channel_difference: 5,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 40169, максимум 0
    AssTolerance {
        name: "ass_blur_drawing",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 5 px из 9781, максимум 6
    AssTolerance {
        name: "ass_blur_frz",
        differing_pixels: 8,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 6483, максимум 9
    AssTolerance {
        name: "ass_blur_gauss_and_be",
        differing_pixels: 12,
        max_channel_difference: 11,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 9 px из 32211, максимум 4
    AssTolerance {
        name: "ass_blur_gauss_border",
        differing_pixels: 14,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 23628, максимум 0
    AssTolerance {
        name: "ass_blur_gauss_noborder",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 32 px из 14225, максимум 128
    AssTolerance {
        name: "ass_blur_runs",
        differing_pixels: 45,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 16 px из 25468, максимум 7
    AssTolerance {
        name: "ass_blur_scaled_storage",
        differing_pixels: 23,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 2 px из 13691, максимум 3
    AssTolerance {
        name: "ass_blur_shadow",
        differing_pixels: 4,
        max_channel_difference: 5,
        area_percent_limit: ONE_PERCENT,
    },
];

/// Кейсы клипов `\clip`/`\iclip` и расстояния камеры при размере хранения, не равном кадру
/// (измерения 2026-10-17, libass 0.17.5).
///
/// Число, порядок, положение, размер и цвет картинок совпадают с libass точно (в том числе
/// четыре куска инверсного клипа); записи libass нулевого размера (`w = h = 0`, так он
/// «удаляет» картинку векторным клипом) сравнение пропускает, у нас пустых картинок нет.
/// Рисунки под клипом совпадают бит в бит. Остаток в кейсах с буквами — шум растеризации
/// букв слоя 1; «максимум 255» и «128» — пиксели с альфой 0 и 1 (цвет почти прозрачного
/// пикселя не имеет значения), см. [`super::ass_tolerances_layer2`].
pub(super) const LAYER3_CLIP_CASES: &[AssTolerance] = &[
    // измерено: 5 px из 16580 (0.03 %), максимум 5
    AssTolerance {
        name: "ass_clip_background_box",
        differing_pixels: 8,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 39 px из 11206 (0.35 %), максимум 6
    AssTolerance {
        name: "ass_clip_border_shadow_blur",
        differing_pixels: 47,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 8 px из 3915 (0.20 %), максимум 4
    AssTolerance {
        name: "ass_clip_collision",
        differing_pixels: 12,
        max_channel_difference: 6,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 0 px из 50404 (0.00 %), максимум 0
    AssTolerance {
        name: "ass_clip_drawing",
        differing_pixels: 0,
        max_channel_difference: 0,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 15 px из 3458 (0.43 %), максимум 255
    // Маска 30000 x 15080 px: мы растеризуем только часть под холстом, но по сетке тайлов всей
    // фигуры libass, поэтому края маски и размеры картинок совпадают с libass точно. Остаток -
    // шум растеризации букв слоя 1 (максимум 255 - пиксели с альфой 0 и 1).
    AssTolerance {
        name: "ass_clip_vector_huge",
        differing_pixels: 19,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 25 px из 4238 (0.59 %), максимум 255
    AssTolerance {
        name: "ass_clip_multiple",
        differing_pixels: 31,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 33 px из 5470 (0.60 %), максимум 255
    AssTolerance {
        name: "ass_clip_outside_frame",
        differing_pixels: 40,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 42 px из 5791 (0.73 %), максимум 255
    AssTolerance {
        name: "ass_clip_playres_scale",
        differing_pixels: 51,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 62 px из 7971 (0.78 %), максимум 128
    AssTolerance {
        name: "ass_clip_rect",
        differing_pixels: 74,
        max_channel_difference: 130,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 47 px из 9854 (0.48 %), максимум 255
    AssTolerance {
        name: "ass_clip_rect_inverse",
        differing_pixels: 57,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 40 px из 5478 (0.73 %), максимум 255
    AssTolerance {
        name: "ass_clip_rotated",
        differing_pixels: 48,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 38 px из 5558 (0.68 %), максимум 255
    AssTolerance {
        name: "ass_clip_vector",
        differing_pixels: 46,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 34 px из 5648 (0.60 %), максимум 255
    AssTolerance {
        name: "ass_clip_vector_and_rect",
        differing_pixels: 42,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 20 px из 5341 (0.37 %), максимум 6
    AssTolerance {
        name: "ass_clip_vector_empty",
        differing_pixels: 25,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 76 px из 10684 (0.71 %), максимум 255
    AssTolerance {
        name: "ass_clip_vector_inverse",
        differing_pixels: 90,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 70 px из 8291 (0.84 %), максимум 255
    // Со старой константой камеры (20000 без `blur_scale_y`) кейс не проходит по геометрии:
    // 7969 px отличаются, позиция (501, 319) против (510, 315), размер 363x93 против 400x98.
    AssTolerance {
        name: "ass_rot_camera_storage",
        differing_pixels: 82,
        max_channel_difference: 255,
        area_percent_limit: ONE_PERCENT,
    },
];
