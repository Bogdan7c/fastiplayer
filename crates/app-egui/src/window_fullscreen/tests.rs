use super::test_support::{FakeFullscreenWindow, FakeWindowRequest};
use super::*;

/// `--fullscreen` и пересланный флаг: обычное окно уходит в фуллскрин без рамки.
#[test]
fn enter_requests_borderless_fullscreen_for_a_windowed_window() {
    let window = FakeFullscreenWindow::default();

    assert_eq!(enter_fullscreen(&window), FullscreenEntry::Requested);

    assert!(window.is_fullscreen());
    assert_eq!(window.requests(), [FakeWindowRequest::BorderlessFullscreen]);
}

/// Повторный `fastiplayer --fullscreen film.mkv` при уже полноэкранном окне не выключает
/// фуллскрин (как сделало бы переключение) и ничего не просит у системы.
#[test]
fn enter_leaves_an_already_fullscreen_window_untouched() {
    let window = FakeFullscreenWindow::already_fullscreen();

    assert_eq!(
        enter_fullscreen(&window),
        FullscreenEntry::AlreadyFullscreen
    );

    assert!(window.is_fullscreen());
    assert!(window.requests().is_empty());
}

/// Кнопка/F/двойной клик: переключение туда и обратно тем же входом, что у флага.
#[test]
fn toggle_enters_through_the_same_request_and_leaves_on_second_press() {
    let window = FakeFullscreenWindow::default();

    assert_eq!(toggle_fullscreen(&window), FullscreenToggle::Entered);
    assert_eq!(toggle_fullscreen(&window), FullscreenToggle::Left);

    assert!(!window.is_fullscreen());
    assert_eq!(
        window.requests(),
        [
            FakeWindowRequest::BorderlessFullscreen,
            FakeWindowRequest::LeaveFullscreen
        ]
    );
}
