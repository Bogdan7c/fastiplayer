//! Раскладка «♪» рядом с CC: порядок, зазоры, приоритет CC при сужении окна и неизменность
//! прежней строки, когда кнопки нет.

use egui::Rect;

use super::super::layout::{AudioTrackControlMode, choose_mode, layout};
use super::{geometry_for_window_width, neighbor_for_width};
use crate::ui::player_controls::subtitle_control::{
    TestSubtitleControlMode, choose_subtitle_mode, layout_without_subtitles, subtitle_layout,
};

/// Ширины окна из задания.
const WIDTHS: [f32; 6] = [640.0, 520.0, 480.0, 440.0, 400.0, 1280.0];

fn rightmost(button: Rect, arrow: Option<Rect>) -> Rect {
    arrow.unwrap_or(button)
}

#[test]
fn audio_sits_left_of_cc_with_the_standard_gap_and_never_overlaps_neighbours() {
    for width in (470_u16..1_300).map(f32::from) {
        let neighbor = neighbor_for_width(width, true);
        let mode = choose_mode(&neighbor);
        if mode == AudioTrackControlMode::Hidden {
            continue;
        }
        let geometry = neighbor.geometry;
        let gap = geometry.controls_style.queue_mode_neighbor_gap;
        let audio = layout(&neighbor, mode, 1.0);
        let audio_button = audio.button_rect.expect("audio is visible");
        let audio_right = rightmost(audio_button, audio.arrow_rect);
        let cc = subtitle_layout(&geometry, neighbor.mode, 1.0);
        let cc_button = cc.button_rect.expect("CC is visible");

        assert!(
            (cc_button.left() - audio_right.right() - gap).abs() < 0.001,
            "audio to CC gap at {width}"
        );
        assert!(
            audio.queue_mode.repeat_rect.right() + gap <= audio_button.left() + 0.001,
            "Repeat to audio gap at {width}"
        );
        match (mode, audio.arrow_rect) {
            (AudioTrackControlMode::Full, Some(arrow)) => {
                assert_eq!(arrow.left(), audio_button.right(), "arrow touches the note");
            }
            (AudioTrackControlMode::Compact, None) => {}
            other => panic!("mode and arrow disagree at {width}: {other:?}"),
        }
    }
}

#[test]
fn without_subtitles_audio_takes_ccs_place_next_to_fullscreen() {
    let neighbor = neighbor_for_width(800.0, false);
    let mode = choose_mode(&neighbor);
    assert_eq!(mode, AudioTrackControlMode::Full);
    let result = layout(&neighbor, mode, 1.0);
    let right = rightmost(result.button_rect.expect("visible"), result.arrow_rect);
    let gap = neighbor.geometry.controls_style.queue_mode_neighbor_gap;
    assert!((neighbor.geometry.fullscreen_button_rect.left() - right.right() - gap).abs() < 0.001);
}

#[test]
fn audio_hides_before_cc_as_the_window_shrinks() {
    let mut first_width_with_cc = None;
    let mut first_width_with_audio = None;
    for width in (400_u16..1_300).map(f32::from) {
        let neighbor = neighbor_for_width(width, true);
        let cc_visible = neighbor.mode != TestSubtitleControlMode::Hidden;
        let audio_visible = choose_mode(&neighbor) != AudioTrackControlMode::Hidden;
        assert!(
            !audio_visible || cc_visible,
            "audio shown without CC at {width}"
        );
        if cc_visible {
            first_width_with_cc.get_or_insert(width);
        }
        if audio_visible {
            first_width_with_audio.get_or_insert(width);
        }
    }
    let with_cc = first_width_with_cc.expect("CC fits somewhere");
    let with_audio = first_width_with_audio.expect("audio fits somewhere");
    assert!(
        with_audio > with_cc,
        "audio appears at {with_audio}, CC at {with_cc}: CC must keep priority"
    );
}

#[test]
fn modes_at_the_requested_widths() {
    let modes: Vec<(f32, AudioTrackControlMode, TestSubtitleControlMode)> = WIDTHS
        .iter()
        .map(|width| {
            let neighbor = neighbor_for_width(*width, true);
            (*width, choose_mode(&neighbor), neighbor.mode)
        })
        .collect();
    assert_eq!(
        modes,
        vec![
            (
                640.0,
                AudioTrackControlMode::Full,
                TestSubtitleControlMode::Full
            ),
            (
                520.0,
                AudioTrackControlMode::Hidden,
                TestSubtitleControlMode::Full
            ),
            (
                480.0,
                AudioTrackControlMode::Hidden,
                TestSubtitleControlMode::Compact
            ),
            (
                440.0,
                AudioTrackControlMode::Hidden,
                TestSubtitleControlMode::Hidden
            ),
            (
                400.0,
                AudioTrackControlMode::Hidden,
                TestSubtitleControlMode::Hidden
            ),
            (
                1280.0,
                AudioTrackControlMode::Full,
                TestSubtitleControlMode::Full
            ),
        ]
    );
}

#[test]
fn audio_thresholds_are_computed_from_the_same_formulas_as_the_layout() {
    // Рядом с CC «♪» без стрелки помещается с 586 pt окна, со стрелкой — с 614 pt.
    let first_width = |wanted: AudioTrackControlMode| {
        (400_u16..1_300)
            .map(f32::from)
            .find(|width| choose_mode(&neighbor_for_width(*width, true)) == wanted)
    };
    assert_eq!(first_width(AudioTrackControlMode::Compact), Some(586.0));
    assert_eq!(first_width(AudioTrackControlMode::Full), Some(614.0));
    // Выше порога режим не откатывается назад.
    for width in (614_u16..1_300).map(f32::from) {
        assert_eq!(
            choose_mode(&neighbor_for_width(width, true)),
            AudioTrackControlMode::Full
        );
    }
}

#[test]
fn one_audio_track_leaves_the_row_exactly_as_it_was_with_only_cc() {
    for width in WIDTHS {
        // С субтитрами: CC полностью показана, «♪» не показывается (прогресс 0).
        let neighbor = neighbor_for_width(width, true);
        let cc_alone = subtitle_layout(&neighbor.geometry, neighbor.mode, 1.0);
        let with_hidden_audio = layout(&neighbor, choose_mode(&neighbor), 0.0);
        assert_eq!(with_hidden_audio.button_rect, None, "{width}");
        assert_eq!(with_hidden_audio.arrow_rect, None, "{width}");
        assert_eq!(with_hidden_audio.queue_mode, cc_alone.queue_mode, "{width}");

        // Без субтитров: строка как до появления обеих кнопок.
        let bare = neighbor_for_width(width, false);
        let bare_layout = layout(&bare, choose_mode(&bare), 0.0);
        assert_eq!(
            bare_layout.queue_mode,
            layout_without_subtitles(&bare.geometry),
            "{width}"
        );
    }
}

#[test]
fn cc_rects_do_not_depend_on_the_audio_button() {
    // CC считается только по геометрии и своему режиму: наличие «♪» её не двигает.
    for width in WIDTHS {
        let geometry = geometry_for_window_width(width);
        let mode = choose_subtitle_mode(&geometry);
        let cc = subtitle_layout(&geometry, mode, 1.0);
        let neighbor = neighbor_for_width(width, true);
        assert_eq!(cc, subtitle_layout(&neighbor.geometry, neighbor.mode, 1.0));
    }
}

#[test]
fn appearing_audio_shrinks_the_queue_controls_smoothly_without_overlap() {
    let neighbor = neighbor_for_width(620.0, true);
    let mode = choose_mode(&neighbor);
    let gap = neighbor.geometry.controls_style.queue_mode_neighbor_gap;
    let play_x = neighbor.geometry.playback_button_rect.center().x;
    let distances: Vec<f32> = (0..=10)
        .map(|step| {
            let result = layout(&neighbor, mode, step as f32 / 10.0);
            // Во время появления кнопка почти прозрачна и стоит в конечной позиции (как CC), так
            // что строгий зазор проверяем в конце анимации.
            if let (Some(button), 10) = (result.button_rect, step) {
                assert!(
                    result.queue_mode.repeat_rect.right() + gap <= button.left() + 0.001,
                    "Repeat overlaps audio at step {step}"
                );
            }
            result.queue_mode.repeat_rect.center().x - play_x
        })
        .collect();
    assert!(
        distances.windows(2).all(|pair| pair[1] <= pair[0] + 0.001),
        "{distances:?}"
    );
    assert!(
        distances[10] < distances[0],
        "audio must take room at 620 pt"
    );
    assert!(distances.windows(2).all(|pair| pair[0] - pair[1] < 12.0));
}

#[test]
fn next_keeps_its_distance_from_repeat_with_both_buttons_visible() {
    for width in [520.0, 640.0, 1280.0] {
        let neighbor = neighbor_for_width(width, true);
        let result = layout(&neighbor, choose_mode(&neighbor), 1.0);
        let geometry = neighbor.geometry;
        assert!(
            geometry.base_next_button_rect.right()
                + geometry.controls_style.queue_mode_neighbor_gap
                <= result.queue_mode.repeat_rect.left() + 0.001,
            "Next vs Repeat at {width}"
        );
    }
}
