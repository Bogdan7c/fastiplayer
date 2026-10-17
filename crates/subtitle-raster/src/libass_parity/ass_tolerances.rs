//! Таблица допусков паритета ASS-событий с libass 0.17.5.
//!
//! Допуск каждого кейса — измеренное значение с запасом ~30 %, а не общий предел. Кейсы
//! добавляются вместе со строкой: `every_ass_case_has_a_measured_tolerance` не даёт забыть.

/// Общий предел площади расхождений (решение владельца): 1 % закрашенной libass площади.
pub(super) const ONE_PERCENT: f64 = 1.0;

/// Допуск кейса: измеренные значения с запасом.
pub(super) struct AssTolerance {
    pub(super) name: &'static str,
    /// Пикселей композита с разницей канала > 2 (измерено; запас ~25 %).
    pub(super) differing_pixels: usize,
    /// Наибольшая разница канала (измерено; запас).
    pub(super) max_channel_difference: u8,
    /// Предел доли отличающихся пикселей от площади libass, %.
    pub(super) area_percent_limit: f64,
}

/// Кейсы ASS-событий. Измерения сняты 2026-10-17 на libass 0.17.5.
pub(super) const ASS_CASES: &[AssTolerance] = &[
    // измерено: 63 px из 13561 (0.46 %), максимум 26
    AssTolerance {
        name: "ass_bold",
        differing_pixels: 84,
        max_channel_difference: 30,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 102 px из 33249 (0.31 %), максимум 24
    AssTolerance {
        name: "ass_border_shadow_tags",
        differing_pixels: 135,
        max_channel_difference: 28,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 6 px из 26259 (0.02 %), максимум 5
    AssTolerance {
        name: "ass_border_style3_pos",
        differing_pixels: 10,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 32 px из 37026 (0.09 %), максимум 7
    AssTolerance {
        name: "ass_border_style4_pos",
        differing_pixels: 44,
        max_channel_difference: 11,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 25 px из 10631 (0.24 %), максимум 5
    AssTolerance {
        name: "ass_collision_two_events",
        differing_pixels: 35,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 42 px из 30122 (0.14 %), максимум 19
    AssTolerance {
        name: "ass_colors_alpha",
        differing_pixels: 57,
        max_channel_difference: 23,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 26 px из 6074 (0.43 %), максимум 11
    AssTolerance {
        name: "ass_font_name",
        differing_pixels: 36,
        max_channel_difference: 15,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 55 px из 11973 (0.46 %), максимум 5
    AssTolerance {
        name: "ass_font_size",
        differing_pixels: 74,
        max_channel_difference: 9,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 13 px из 9427 (0.14 %), максимум 4
    AssTolerance {
        name: "ass_layers",
        differing_pixels: 19,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 122 px из 25135 (0.49 %), максимум 6
    AssTolerance {
        name: "ass_margins_event",
        differing_pixels: 161,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 46 px из 4824 (0.95 %), максимум 6
    AssTolerance {
        name: "ass_no_playres",
        differing_pixels: 62,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 63 px из 13271 (0.47 %), максимум 28
    AssTolerance {
        name: "ass_playres_1920x1080",
        differing_pixels: 84,
        max_channel_difference: 32,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 25 px из 9397 (0.27 %), максимум 4
    AssTolerance {
        name: "ass_playres_384x288",
        differing_pixels: 35,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 57 px из 9388 (0.61 %), максимум 7
    AssTolerance {
        name: "ass_pos_alignments",
        differing_pixels: 77,
        max_channel_difference: 11,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 41 px из 8009 (0.51 %), максимум 7
    AssTolerance {
        name: "ass_reset_named_style",
        differing_pixels: 56,
        max_channel_difference: 11,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 29 px из 13919 (0.21 %), максимум 6
    AssTolerance {
        name: "ass_scaled_border_no_storage",
        differing_pixels: 40,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 19 px из 16376 (0.12 %), максимум 6
    AssTolerance {
        name: "ass_scaled_border_yes_storage",
        differing_pixels: 27,
        max_channel_difference: 10,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 51 px из 14373 (0.35 %), максимум 9
    AssTolerance {
        name: "ass_storage_aspect",
        differing_pixels: 69,
        max_channel_difference: 13,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 50 px из 10935 (0.46 %), максимум 4
    AssTolerance {
        name: "ass_unpositioned_alignments",
        differing_pixels: 67,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 89 px из 32382 (0.27 %), максимум 16
    AssTolerance {
        name: "ass_wrap_q0",
        differing_pixels: 118,
        max_channel_difference: 20,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 86 px из 32395 (0.27 %), максимум 17
    AssTolerance {
        name: "ass_wrap_q1",
        differing_pixels: 114,
        max_channel_difference: 21,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 67 px из 29412 (0.23 %), максимум 15
    AssTolerance {
        name: "ass_wrap_q2",
        differing_pixels: 90,
        max_channel_difference: 19,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 99 px из 10191 (0.97 %), максимум 32
    AssTolerance {
        name: "ass_scale_style",
        differing_pixels: 129,
        max_channel_difference: 42,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 43 px из 9599 (0.45 %), максимум 23
    AssTolerance {
        name: "ass_scale_tags",
        differing_pixels: 56,
        max_channel_difference: 30,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 100 px из 14176 (0.71 %), максимум 21
    AssTolerance {
        name: "ass_spacing_tags",
        differing_pixels: 130,
        max_channel_difference: 28,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 59 px из 8824 (0.67 %), максимум 25
    AssTolerance {
        name: "ass_spacing_style",
        differing_pixels: 77,
        max_channel_difference: 33,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 41 px из 10934 (0.37 %), максимум 6
    AssTolerance {
        name: "ass_spacing_ligatures",
        differing_pixels: 54,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 10 px из 7297 (0.14 %), максимум 6
    AssTolerance {
        name: "ass_kerning_off",
        differing_pixels: 13,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 11 px из 7257 (0.15 %), максимум 5
    AssTolerance {
        name: "ass_kerning_on",
        differing_pixels: 15,
        max_channel_difference: 7,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 75 px из 13961 (0.54 %), максимум 24
    AssTolerance {
        name: "ass_italic_synth",
        differing_pixels: 98,
        max_channel_difference: 32,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 22 px из 15443 (0.14 %), максимум 6
    AssTolerance {
        name: "ass_bold_synth",
        differing_pixels: 29,
        max_channel_difference: 8,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 44 px из 21344 (0.21 %), максимум 9
    AssTolerance {
        name: "ass_bold_synth_georgian",
        differing_pixels: 58,
        max_channel_difference: 12,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 59 px из 11022 (0.54 %), максимум 23
    AssTolerance {
        name: "ass_underline_strike",
        differing_pixels: 77,
        max_channel_difference: 30,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 37 px из 31106 (0.12 %), максимум 21
    AssTolerance {
        name: "ass_underline_border_shadow",
        differing_pixels: 49,
        max_channel_difference: 28,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 181 px из 47171 (0.38 %), максимум 64
    AssTolerance {
        name: "ass_ssa_v4_legacy",
        differing_pixels: 236,
        max_channel_difference: 84,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 252 px из 52931 (0.48 %), максимум 31
    AssTolerance {
        name: "ass_fansub_dialogue_1080",
        differing_pixels: 328,
        max_channel_difference: 41,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 209 px из 72313 (0.29 %), максимум 12
    AssTolerance {
        name: "ass_fansub_signs_360",
        differing_pixels: 272,
        max_channel_difference: 16,
        area_percent_limit: ONE_PERCENT,
    },
    // измерено: 93 px из 40476 (0.23 %), максимум 15
    AssTolerance {
        name: "ass_fansub_mixed_1080",
        differing_pixels: 121,
        max_channel_difference: 20,
        area_percent_limit: ONE_PERCENT,
    },
];
