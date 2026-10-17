//! Тесты прокрутки: пересчёт задержки, положение блока, полоса `Scroll up/down`.

use subtitle_core::{AssBannerDirection, AssScrollDirection};

use super::*;
use crate::ass::options::AssStorageSize;
use crate::ass::test_script::script_info;
use crate::overlay::CanvasSize;
use subtitle_core::AssScaledBorderAndShadow;

/// Кадр: сценарий 384x288 на холсте 1152x864 (множитель 3), хранение неизвестно.
fn frame_3x() -> AssFrameScale {
    let info = script_info((384, 288), AssScaledBorderAndShadow::Enabled);
    AssFrameScale::new(
        &info,
        CanvasSize::new(1152, 864).expect("canvas"),
        AssStorageSize::Unknown,
    )
}

fn banner(delay_ms: i32, direction: AssBannerDirection) -> AssEffect {
    AssEffect::Banner {
        delay_ms,
        direction,
    }
}

#[test]
fn delay_is_truncated_in_storage_pixels_with_a_floor_of_one() {
    // Хранение 768x576 при сценарии 384x288: scale = 2.
    let info = script_info((384, 288), AssScaledBorderAndShadow::Enabled);
    let frame = AssFrameScale::new(
        &info,
        CanvasSize::new(768, 576).expect("canvas"),
        AssStorageSize::from_pixels(768, 576),
    );
    let delay_of = |written: i32| {
        ScrollClock::for_effect(banner(written, AssBannerDirection::RightToLeft), &frame)
            .map(|clock| clock.delay_ms)
    };
    // 7 / 2 = 3.5 -> 3 -> 3 * 2 = 6.
    assert_eq!(delay_of(7), Some(6.0));
    // 0 и отрицательные -> max(.., 1) -> 1 * 2.
    assert_eq!(delay_of(0), Some(2.0));
    assert_eq!(delay_of(-5), Some(2.0));
    // Без пересчёта малых значений: 10 / 2 = 5 -> 10.
    assert_eq!(delay_of(10), Some(10.0));
}

#[test]
fn vertical_scroll_uses_the_vertical_scale_for_the_delay() {
    // Раскладка 768x288: по горизонтали масштаб 2, по вертикали 1.
    let info = script_info((384, 288), AssScaledBorderAndShadow::Enabled);
    let frame = AssFrameScale::new(
        &info,
        CanvasSize::new(768, 288).expect("canvas"),
        AssStorageSize::from_pixels(768, 288),
    );
    let scroll = AssEffect::Scroll {
        direction: AssScrollDirection::Up,
        y_top: 0,
        y_bottom: 288,
        delay_ms: 7,
    };
    let clock = ScrollClock::for_effect(scroll, &frame).expect("scroll clock");
    assert_eq!(clock.delay_ms, 7.0);
    assert_eq!(clock.shift_after(70), 10.0);
}

#[test]
fn no_clock_without_an_effect() {
    assert_eq!(ScrollClock::for_effect(AssEffect::None, &frame_3x()), None);
}

#[test]
fn banner_directions_place_the_block_by_their_own_edge() {
    let frame = frame_3x();
    // Справа налево: начало блока на PlayResX - shift = 384 - 10 = 374 -> 1122 px.
    assert_eq!(
        scroll_placement(banner(10, AssBannerDirection::RightToLeft), 10.0, &frame),
        ScrollPlacement::RightToLeft { origin_x: 1122.0 }
    );
    // Слева направо: правый край на shift = 10 -> 30 px.
    assert_eq!(
        scroll_placement(banner(10, AssBannerDirection::LeftToRight), 10.0, &frame),
        ScrollPlacement::LeftToRight { right_edge_x: 30.0 }
    );
}

#[test]
fn scroll_directions_move_toward_the_opposite_band_edge() {
    let frame = frame_3x();
    let scroll = |direction| AssEffect::Scroll {
        direction,
        y_top: 50,
        y_bottom: 250,
        delay_ms: 10,
    };
    // Вниз: нижний край блока на y_top + shift = 60 -> 180 px.
    assert_eq!(
        scroll_placement(scroll(AssScrollDirection::Down), 10.0, &frame),
        ScrollPlacement::TopToBottom {
            bottom_edge_y: 180.0
        }
    );
    // Вверх: верхний край блока на y_bottom - shift = 240 -> 720 px.
    assert_eq!(
        scroll_placement(scroll(AssScrollDirection::Up), 10.0, &frame),
        ScrollPlacement::BottomToTop { top_edge_y: 720.0 }
    );
    assert_eq!(
        scroll_placement(AssEffect::None, 10.0, &frame),
        ScrollPlacement::None
    );
}

#[test]
fn band_exists_only_for_vertical_scroll_and_is_rounded_to_pixels() {
    let info = script_info((100, 100), AssScaledBorderAndShadow::Enabled);
    let frame = AssFrameScale::new(
        &info,
        CanvasSize::new(150, 150).expect("canvas"),
        AssStorageSize::Unknown,
    );
    let scroll = AssEffect::Scroll {
        direction: AssScrollDirection::Down,
        y_top: 11,
        y_bottom: 33,
        delay_ms: 5,
    };
    // 11 * 1.5 = 16.5 -> 17 (от нуля); 33 * 1.5 = 49.5 -> 50.
    assert_eq!(
        scroll_band(scroll, &frame),
        Some(ScrollBand {
            top: 17,
            bottom: 50
        })
    );
    assert_eq!(
        scroll_band(banner(5, AssBannerDirection::RightToLeft), &frame),
        None
    );
    assert_eq!(scroll_band(AssEffect::None, &frame), None);
}
