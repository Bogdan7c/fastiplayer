//! Раскладка CC: режимы по ширине, симметрия Shuffle/Repeat, зазоры и неизменность
//! прежней раскладки, когда кнопки нет.

use egui::Rect;

use super::super::layout::{
    SubtitleControlMode, SubtitleLayoutGeometry, choose_mode, layout, layout_without_subtitles,
};
use super::geometry_for_window_width;
use crate::ui::player_controls::playback_rate;

/// Ширины окна из задания и по обе стороны вычисленных порогов.
const WIDTHS: [f32; 5] = [400.0, 442.0, 500.0, 640.0, 1280.0];

/// Первая ширина окна (целые points), при которой режим не `Hidden` / равен `Full`.
fn first_width_with(predicate: impl Fn(SubtitleControlMode) -> bool) -> f32 {
    (300_u16..1_000)
        .map(f32::from)
        .find(|width| predicate(choose_mode(&geometry_for_window_width(*width))))
        .expect("some width fits")
}

fn distance_from_play(geometry: &SubtitleLayoutGeometry, rect: Rect) -> f32 {
    rect.center().x - geometry.playback_button_rect.center().x
}

#[test]
fn without_tracks_the_row_is_exactly_what_it_was_before_cc_existed() {
    for width in WIDTHS {
        let geometry = geometry_for_window_width(width);
        let mode = choose_mode(&geometry);
        // Нет дорожек -> progress появления остаётся нулём.
        let result = layout(&geometry, mode, 0.0);
        assert_eq!(result.button_rect, None, "no CC rect at {width}");
        assert_eq!(result.arrow_rect, None, "no arrow rect at {width}");
        assert_eq!(result.queue_mode, layout_without_subtitles(&geometry));

        // Числа «как сегодня»: расстояние Shuffle/Repeat = min(156, половина строки - 73).
        let expected_distance = (156.0_f32).min((width - 20.0) / 2.0 - 73.0);
        let distance = distance_from_play(&geometry, result.queue_mode.repeat_rect);
        assert!(
            (distance - expected_distance).abs() < 0.001,
            "{width}: {distance} != {expected_distance}"
        );
    }
}

#[test]
fn modes_follow_the_width_with_the_computed_thresholds() {
    let modes: Vec<(f32, SubtitleControlMode)> = WIDTHS
        .iter()
        .map(|width| (*width, choose_mode(&geometry_for_window_width(*width))))
        .collect();
    assert_eq!(
        modes,
        vec![
            (400.0, SubtitleControlMode::Hidden),
            (442.0, SubtitleControlMode::Hidden),
            (500.0, SubtitleControlMode::Full),
            (640.0, SubtitleControlMode::Full),
            (1280.0, SubtitleControlMode::Full),
        ]
    );

    // Пороги вычислены из тех же формул: Compact с 470 pt, Full с 498 pt окна.
    let compact_from = first_width_with(|mode| mode != SubtitleControlMode::Hidden);
    let full_from = first_width_with(|mode| mode == SubtitleControlMode::Full);
    assert_eq!(compact_from, 470.0);
    assert_eq!(full_from, 498.0);
    // Выше порога режим не откатывается назад.
    for width in (470_u16..1_300).map(f32::from) {
        let mode = choose_mode(&geometry_for_window_width(width));
        let expected = if width >= full_from {
            SubtitleControlMode::Full
        } else {
            SubtitleControlMode::Compact
        };
        assert_eq!(mode, expected, "at {width}");
    }
}

#[test]
fn visible_cc_never_overlaps_neighbours_and_keeps_twelve_point_gaps() {
    for width in [470.0, 497.0, 498.0, 500.0, 640.0, 1280.0] {
        let geometry = geometry_for_window_width(width);
        let mode = choose_mode(&geometry);
        assert_ne!(mode, SubtitleControlMode::Hidden, "{width}");
        let result = layout(&geometry, mode, 1.0);
        let button = result.button_rect.expect("CC is visible");
        let gap = geometry.controls_style.queue_mode_neighbor_gap;

        assert!(
            result.queue_mode.repeat_rect.right() + gap <= button.left() + 0.001,
            "Repeat to CC gap at {width}"
        );
        let rightmost = result.arrow_rect.unwrap_or(button);
        assert!(
            (geometry.fullscreen_button_rect.left() - rightmost.right() - gap).abs() < 0.001,
            "CC to Fullscreen gap at {width}"
        );
        match (mode, result.arrow_rect) {
            (SubtitleControlMode::Full, Some(arrow)) => {
                assert_eq!(arrow.left(), button.right(), "arrow touches CC");
                assert_eq!(
                    arrow.width(),
                    geometry.controls_style.subtitles_menu_arrow_width
                );
            }
            (SubtitleControlMode::Compact, None) => {}
            other => panic!("mode and arrow disagree at {width}: {other:?}"),
        }
        assert_eq!(
            button.size().x,
            geometry.controls_style.subtitles_button_size
        );
    }
}

#[test]
fn shuffle_stays_mirrored_and_next_keeps_its_distance_from_repeat() {
    for width in WIDTHS.into_iter().chain([470.0, 498.0]) {
        let geometry = geometry_for_window_width(width);
        let mode = choose_mode(&geometry);
        for reveal in [0.0, 0.5, 1.0] {
            let result = layout(&geometry, mode, reveal);
            let play_x = geometry.playback_button_rect.center().x;
            assert!(
                ((play_x - result.queue_mode.shuffle_rect.center().x)
                    - (result.queue_mode.repeat_rect.center().x - play_x))
                    .abs()
                    < 0.001,
                "mirror at {width}/{reveal}"
            );
            // Next в любом положении кнопки скорости остаётся на 12 pt левее Repeat.
            for rate_reveal in [0.0, 0.5, 1.0] {
                let rate_layout = playback_rate::control_layout(
                    geometry.playback_button_rect,
                    geometry.base_next_button_rect,
                    result.queue_mode.repeat_rect,
                    geometry.controls_style,
                    geometry.item_spacing,
                    rate_reveal,
                );
                assert!(
                    rate_layout.next_button_rect.right()
                        + geometry.controls_style.queue_mode_neighbor_gap
                        <= result.queue_mode.repeat_rect.left() + 0.001,
                    "Next vs Repeat at {width}, cc {reveal}, rate {rate_reveal}"
                );
            }
        }
    }
}

#[test]
fn wide_windows_do_not_move_anything_when_cc_appears() {
    for width in [640.0, 1280.0] {
        let geometry = geometry_for_window_width(width);
        let mode = choose_mode(&geometry);
        for reveal in [0.0, 0.5, 1.0] {
            assert_eq!(
                layout(&geometry, mode, reveal).queue_mode,
                layout_without_subtitles(&geometry),
                "{width} at reveal {reveal}"
            );
        }
    }
}

#[test]
fn appearing_cc_shrinks_the_distance_smoothly_and_never_jumps_back() {
    let geometry = geometry_for_window_width(500.0);
    let mode = choose_mode(&geometry);
    let distances: Vec<f32> = (0..=10)
        .map(|step| {
            let result = layout(&geometry, mode, step as f32 / 10.0);
            distance_from_play(&geometry, result.queue_mode.repeat_rect)
        })
        .collect();
    assert!(
        distances.windows(2).all(|pair| pair[1] <= pair[0] + 0.001),
        "distance must only shrink while CC appears: {distances:?}"
    );
    assert!(distances[10] < distances[0], "CC must take room at 500 pt");
    // Шаги малы: ничего не прыгает.
    assert!(distances.windows(2).all(|pair| pair[0] - pair[1] < 12.0));
}

#[test]
fn hidden_mode_reserves_nothing_even_for_a_fully_revealed_request() {
    let geometry = geometry_for_window_width(400.0);
    let result = layout(&geometry, SubtitleControlMode::Hidden, 1.0);
    assert_eq!(result.button_rect, None);
    assert_eq!(result.arrow_rect, None);
    assert_eq!(result.queue_mode, layout_without_subtitles(&geometry));
}
