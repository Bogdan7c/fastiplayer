//! Раскладка кнопки качества: порядок в строке, зазоры, приоритет соседей и неизменность
//! прежней строки, когда кнопки нет.

use super::super::layout::{QualityControlMode, choose_mode, layout};
use super::{TestAudioTrackControlMode, chain};
use crate::ui::player_controls::audio_track_control::audio_layout;
use crate::ui::player_controls::subtitle_control::subtitle_layout;

/// Кнопка стоит левее «♪» и CC с обычным зазором и не наезжает на Repeat.
#[test]
fn quality_sits_left_of_audio_and_cc_with_the_standard_gap() {
    for width in (480_u16..1_500).map(f32::from) {
        let chain = chain(width, true, true);
        let mode = choose_mode(&chain.quality);
        if mode == QualityControlMode::Hidden {
            continue;
        }
        let geometry = chain.subtitle.geometry;
        let gap = geometry.controls_style.queue_mode_neighbor_gap;
        let quality = layout(&chain.quality, mode, 1.0);
        let quality_rect = quality.button_rect.expect("shown");
        assert_ne!(
            chain.audio_mode,
            TestAudioTrackControlMode::Hidden,
            "{width}"
        );
        let audio = audio_layout(&chain.subtitle, chain.audio_mode, 1.0);
        let audio_left = audio.button_rect.expect("audio shown").left();
        assert!(
            (audio_left - quality_rect.right() - gap).abs() < 0.001,
            "quality to audio gap at {width}"
        );
        assert!(
            quality.queue_mode.repeat_rect.right() + gap <= quality_rect.left() + 0.001,
            "Repeat to quality gap at {width}"
        );
        let cc = subtitle_layout(&geometry, chain.subtitle.mode, 1.0);
        assert!(
            quality_rect.right() < cc.button_rect.expect("CC shown").left(),
            "quality never reaches CC at {width}"
        );
    }
}

/// «♪» может не быть (одна дорожка): кнопка встаёт сразу левее CC, а без CC — левее Fullscreen.
#[test]
fn without_audio_quality_takes_its_place_next_to_cc_or_fullscreen() {
    let with_cc = chain(900.0, true, false);
    let quality = layout(&with_cc.quality, QualityControlMode::Shown, 1.0);
    let gap = with_cc
        .subtitle
        .geometry
        .controls_style
        .queue_mode_neighbor_gap;
    let cc = subtitle_layout(&with_cc.subtitle.geometry, with_cc.subtitle.mode, 1.0);
    let cc_rect = cc.button_rect.expect("CC shown");
    let cc_left = cc_rect.left();
    assert!(
        (cc_left - quality.button_rect.expect("shown").right() - gap).abs() < 0.001,
        "quality is glued to CC with the standard gap"
    );

    let bare = chain(900.0, false, false);
    let alone = layout(&bare.quality, QualityControlMode::Shown, 1.0);
    let fullscreen_left = bare.subtitle.geometry.fullscreen_button_rect.left();
    assert!(
        (fullscreen_left - alone.button_rect.expect("shown").right() - gap).abs() < 0.001,
        "without CC and audio quality stands next to Fullscreen"
    );
}

/// Пока кнопка не появилась, Shuffle/Repeat там же, где были без неё (вся строка прежняя).
#[test]
fn while_the_button_is_absent_the_queue_row_is_unchanged() {
    for (with_subtitles, with_audio) in [(true, true), (true, false), (false, false)] {
        let chain = chain(800.0, with_subtitles, with_audio);
        let hidden = layout(&chain.quality, QualityControlMode::Shown, 0.0);
        assert_eq!(hidden.button_rect, None);
        let audio_progress = if with_audio { 1.0 } else { 0.0 };
        let before = audio_layout(&chain.subtitle, chain.audio_mode, audio_progress);
        assert_eq!(hidden.queue_mode, before.queue_mode);
    }
}

/// Кнопка занимает место: с её появлением Shuffle/Repeat сжимаются, а не наезжают.
#[test]
fn the_button_pushes_the_queue_buttons_in() {
    let chain = chain(700.0, true, true);
    let without = layout(&chain.quality, QualityControlMode::Shown, 0.0);
    let with = layout(&chain.quality, QualityControlMode::Shown, 1.0);
    assert!(with.queue_mode.repeat_rect.left() <= without.queue_mode.repeat_rect.left());
    assert!(
        with.queue_mode.repeat_rect.right() <= without.queue_mode.repeat_rect.right() - 1.0
            || with.queue_mode.repeat_rect == without.queue_mode.repeat_rect,
        "the pair never drifts outwards"
    );
}

/// При сужении окна кнопка качества исчезает раньше «♪» и CC.
#[test]
fn quality_gives_way_before_audio_and_cc() {
    let mut saw_quality_hidden_while_audio_shown = false;
    for width in (400_u16..1_500).map(f32::from) {
        let chain = chain(width, true, true);
        let quality_mode = choose_mode(&chain.quality);
        let audio_shown = chain.audio_mode != TestAudioTrackControlMode::Hidden;
        if quality_mode == QualityControlMode::Shown {
            assert!(
                audio_shown,
                "quality is shown while audio is hidden at {width}"
            );
        } else if audio_shown {
            saw_quality_hidden_while_audio_shown = true;
        }
    }
    assert!(
        saw_quality_hidden_while_audio_shown,
        "somewhere the window is wide enough for the audio button but not for quality"
    );
}
