//! Автоскрытие chrome на фальшивом времени: когда прячется, что возвращает, что удерживает.

use std::time::{Duration, Instant};

use player_core::PlaybackState;

use super::*;
use crate::ui::test_frame::{input_at, run_ui_frame};

const DELAY: Duration = Duration::from_millis(2500);
const SLIDE_SECONDS: f32 = 0.5;
const FRAME: Duration = Duration::from_millis(16);

/// Тестовый «кадровый» драйвер контроллера с управляемыми часами.
struct Harness {
    controller: FullscreenChromeController,
    now: Instant,
    mode: WindowPresentationMode,
    delay: AutohideDelay,
    slide_seconds: f32,
}

impl Harness {
    fn fullscreen() -> Self {
        Self {
            controller: FullscreenChromeController::default(),
            now: Instant::now(),
            mode: WindowPresentationMode::Fullscreen,
            delay: AutohideDelay::After(DELAY),
            slide_seconds: SLIDE_SECONDS,
        }
    }

    /// Один кадр: время идёт на `step`, ввод `activity`.
    fn frame(&mut self, step: Duration, activity: UserActivity) -> ChromePresentation {
        self.now += step;
        self.controller.advance(ChromeFrameInput {
            mode: self.mode,
            activity,
            delay: self.delay,
            slide_duration_seconds: self.slide_seconds,
            now: self.now,
        });
        self.controller.presentation()
    }

    /// Кадры без ввода в течение `duration` с шагом 16 мс.
    fn idle_for(&mut self, duration: Duration) -> ChromePresentation {
        let mut presentation = self.frame(Duration::ZERO, UserActivity::Idle);
        let mut elapsed = Duration::ZERO;
        while elapsed < duration {
            presentation = self.frame(FRAME, UserActivity::Idle);
            elapsed += FRAME;
        }
        presentation
    }
}

fn fully_visible(presentation: ChromePresentation) -> bool {
    presentation.hidden_fraction == 0.0 && presentation.cursor == CursorVisibility::Visible
}

fn fully_hidden(presentation: ChromePresentation) -> bool {
    presentation.hidden_fraction == 1.0 && presentation.cursor == CursorVisibility::Hidden
}

#[test]
fn fullscreen_without_input_hides_panels_and_cursor_after_delay() {
    let mut harness = Harness::fullscreen();
    let started_at = harness.now;
    harness.frame(Duration::ZERO, UserActivity::Detected);

    let before_delay = harness.idle_for(DELAY - Duration::from_millis(100));
    assert!(
        fully_visible(before_delay),
        "до задержки всё видно: {before_delay:?}"
    );
    assert_eq!(
        harness.controller.next_wake_deadline(),
        Some(started_at + DELAY),
        "event loop должен проснуться ровно к моменту скрытия"
    );

    let right_after_delay = harness.idle_for(Duration::from_millis(150));
    assert_eq!(right_after_delay.cursor, CursorVisibility::Hidden);
    assert!(
        right_after_delay.hidden_fraction > 0.0 && right_after_delay.hidden_fraction < 1.0,
        "панели уезжают плавно, а не исчезают: {right_after_delay:?}"
    );
    assert!(harness.controller.is_animating());
    assert_eq!(harness.controller.next_wake_deadline(), None);

    let after_slide = harness.idle_for(Duration::from_secs_f32(SLIDE_SECONDS));
    assert!(fully_hidden(after_slide), "{after_slide:?}");
    assert!(!harness.controller.is_animating());
}

#[test]
fn mouse_movement_brings_chrome_back_and_restarts_delay() {
    let mut harness = Harness::fullscreen();
    harness.idle_for(DELAY + Duration::from_secs(1));
    assert!(fully_hidden(harness.controller.presentation()));

    let after_move = harness.frame(FRAME, UserActivity::Detected);
    assert_eq!(after_move.cursor, CursorVisibility::Visible);
    assert!(
        after_move.hidden_fraction < 1.0,
        "панели начали возвращаться"
    );
    assert_eq!(
        harness.controller.next_wake_deadline(),
        Some(harness.now + DELAY)
    );

    let returned = harness.idle_for(Duration::from_secs_f32(SLIDE_SECONDS));
    assert!(fully_visible(returned), "{returned:?}");

    let hidden_again = harness.idle_for(DELAY);
    assert_eq!(hidden_again.cursor, CursorVisibility::Hidden);
}

#[test]
fn every_hold_reason_keeps_chrome_visible_without_wake_deadline() {
    for reason in [
        ChromeHoldReason::PointerOverChrome,
        ChromeHoldReason::SidebarOpen,
        ChromeHoldReason::PopupOpen,
        ChromeHoldReason::PointerDrag,
        ChromeHoldReason::PlaybackNotRunning,
    ] {
        let mut harness = Harness::fullscreen();
        harness
            .controller
            .record_holds(ChromeHolds::default().with(reason));

        let presentation = harness.idle_for(DELAY * 3);

        assert!(fully_visible(presentation), "{reason:?}: {presentation:?}");
        assert_eq!(
            harness.controller.next_wake_deadline(),
            None,
            "{reason:?}: удержание не должно будить event loop"
        );
    }
}

#[test]
fn released_hold_gives_full_delay_before_hiding() {
    let mut harness = Harness::fullscreen();
    harness
        .controller
        .record_holds(ChromeHolds::default().with(ChromeHoldReason::PopupOpen));
    harness.idle_for(DELAY * 2);

    harness.controller.record_holds(ChromeHolds::default());
    let shortly_after_release = harness.idle_for(DELAY - Duration::from_millis(200));
    assert!(fully_visible(shortly_after_release));

    let after_full_delay = harness.idle_for(Duration::from_millis(300));
    assert_eq!(after_full_delay.cursor, CursorVisibility::Hidden);
}

#[test]
fn windowed_mode_never_hides_and_snaps_back_instantly() {
    let mut harness = Harness::fullscreen();
    harness.mode = WindowPresentationMode::Windowed;
    let windowed = harness.idle_for(DELAY * 3);
    assert!(fully_visible(windowed));
    assert_eq!(harness.controller.next_wake_deadline(), None);

    // Скрыто в фуллскрине, затем выход из него: раскладка окна сразу прежняя, без анимации.
    harness.mode = WindowPresentationMode::Fullscreen;
    harness.idle_for(DELAY + Duration::from_secs(1));
    assert!(fully_hidden(harness.controller.presentation()));
    harness.mode = WindowPresentationMode::Windowed;
    let after_exit = harness.frame(FRAME, UserActivity::Idle);
    assert!(fully_visible(after_exit), "{after_exit:?}");
    assert!(!harness.controller.is_animating());
}

#[test]
fn entering_fullscreen_starts_a_full_delay() {
    let mut harness = Harness::fullscreen();
    harness.mode = WindowPresentationMode::Windowed;
    harness.idle_for(DELAY * 2);

    harness.mode = WindowPresentationMode::Fullscreen;
    let soon_after_enter = harness.idle_for(DELAY - Duration::from_millis(100));

    assert!(fully_visible(soon_after_enter), "{soon_after_enter:?}");
}

#[test]
fn disabled_delay_never_hides() {
    let mut harness = Harness::fullscreen();
    harness.delay = AutohideDelay::Disabled;

    let presentation = harness.idle_for(Duration::from_secs(60));

    assert!(fully_visible(presentation));
    assert_eq!(harness.controller.next_wake_deadline(), None);
}

#[test]
fn reduced_motion_hides_and_shows_without_animation() {
    let mut harness = Harness::fullscreen();
    // sidebar_slide_duration_seconds() отдаёт 0 при reduced motion.
    harness.slide_seconds = 0.0;
    harness.frame(Duration::ZERO, UserActivity::Detected);

    let at_delay = harness.frame(DELAY, UserActivity::Idle);
    assert!(fully_hidden(at_delay), "{at_delay:?}");
    assert!(!harness.controller.is_animating());

    let after_move = harness.frame(FRAME, UserActivity::Detected);
    assert!(fully_visible(after_move), "{after_move:?}");
    assert!(!harness.controller.is_animating());
}

#[test]
fn config_zero_means_disabled_and_other_values_are_milliseconds() {
    assert_eq!(
        AutohideDelay::from_config_millis(0),
        AutohideDelay::Disabled
    );
    assert_eq!(
        AutohideDelay::from_config_millis(2500),
        AutohideDelay::After(Duration::from_millis(2500))
    );
}

#[test]
fn hold_set_tracks_each_reason_independently() {
    let holds = ChromeHolds::default()
        .with(ChromeHoldReason::SidebarOpen)
        .with(ChromeHoldReason::PointerDrag);

    assert!(!holds.is_empty());
    assert!(holds.contains(ChromeHoldReason::SidebarOpen));
    assert!(holds.contains(ChromeHoldReason::PointerDrag));
    assert!(!holds.contains(ChromeHoldReason::PopupOpen));
    assert!(ChromeHolds::default().is_empty());
}

#[test]
fn user_activity_counts_motion_presses_and_typing_but_not_releases_or_leaving() {
    let detected = |events: Vec<egui::Event>| UserActivity::from_raw_input(&input_at(0.0, events));
    let key_event = |pressed| egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };

    assert_eq!(detected(Vec::new()), UserActivity::Idle);
    assert_eq!(
        detected(vec![egui::Event::PointerMoved(egui::pos2(5.0, 5.0))]),
        UserActivity::Detected
    );
    assert_eq!(detected(vec![key_event(true)]), UserActivity::Detected);
    assert_eq!(detected(vec![key_event(false)]), UserActivity::Idle);
    assert_eq!(detected(vec![egui::Event::PointerGone]), UserActivity::Idle);
    assert_eq!(
        detected(vec![egui::Event::MouseMoved(egui::vec2(3.0, 0.0))]),
        UserActivity::Idle
    );
}

/// Прогоняет `finish_frame` в настоящем egui-кадре и возвращает удержания и иконку курсора.
fn finish_real_frame(
    controller: &mut FullscreenChromeController,
    events: Vec<egui::Event>,
    observation_rects: &[egui::Rect],
    sidebar_open: bool,
    playback_state: PlaybackState,
    prepare_ctx: impl Fn(&egui::Context),
) -> egui::CursorIcon {
    let egui_ctx = egui::Context::default();
    let mut cursor_icon = egui::CursorIcon::Default;
    for frame_index in 0..2 {
        let output = run_ui_frame(
            &egui_ctx,
            input_at(f64::from(frame_index), events.clone()),
            |ui| {
                prepare_ctx(ui.ctx());
                controller.finish_frame(
                    ui.ctx(),
                    ChromeHoldObservation {
                        visible_chrome_rects: observation_rects,
                        sidebar_open,
                        playback_state,
                    },
                );
            },
        );
        cursor_icon = output.platform_output.cursor_icon;
    }
    cursor_icon
}

#[test]
fn observation_reports_each_hold_reason_from_real_egui_frame() {
    let titlebar_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 40.0));
    let pointer_on_titlebar = vec![egui::Event::PointerMoved(egui::pos2(100.0, 20.0))];
    let pointer_on_video = vec![egui::Event::PointerMoved(egui::pos2(100.0, 300.0))];
    let no_prepare = |_: &egui::Context| {};

    let mut controller = FullscreenChromeController::default();
    finish_real_frame(
        &mut controller,
        pointer_on_video.clone(),
        &[titlebar_rect],
        false,
        PlaybackState::Playing,
        no_prepare,
    );
    assert!(controller.holds.is_empty(), "{:?}", controller.holds);

    finish_real_frame(
        &mut controller,
        pointer_on_titlebar,
        &[titlebar_rect],
        false,
        PlaybackState::Playing,
        no_prepare,
    );
    assert!(
        controller
            .holds
            .contains(ChromeHoldReason::PointerOverChrome)
    );

    finish_real_frame(
        &mut controller,
        pointer_on_video.clone(),
        &[titlebar_rect],
        true,
        PlaybackState::Buffering,
        no_prepare,
    );
    assert_eq!(
        controller.holds,
        ChromeHolds::default().with(ChromeHoldReason::SidebarOpen),
        "буферизация не держит панели, сайдбар держит"
    );

    finish_real_frame(
        &mut controller,
        pointer_on_video.clone(),
        &[titlebar_rect],
        false,
        PlaybackState::Paused,
        no_prepare,
    );
    assert!(
        controller
            .holds
            .contains(ChromeHoldReason::PlaybackNotRunning)
    );

    finish_real_frame(
        &mut controller,
        pointer_on_video.clone(),
        &[titlebar_rect],
        false,
        PlaybackState::Playing,
        |ctx| egui::Popup::open_id(ctx, egui::Id::new("test_popup")),
    );
    assert!(controller.holds.contains(ChromeHoldReason::PopupOpen));

    finish_real_frame(
        &mut controller,
        pointer_on_video,
        &[titlebar_rect],
        false,
        PlaybackState::Playing,
        |ctx| ctx.set_dragged_id(egui::Id::new("test_drag")),
    );
    assert!(controller.holds.contains(ChromeHoldReason::PointerDrag));
}

#[test]
fn finish_frame_hides_cursor_only_after_hide_decision() {
    let mut harness = Harness::fullscreen();
    harness.frame(Duration::ZERO, UserActivity::Detected);
    let pointer_on_video = vec![egui::Event::PointerMoved(egui::pos2(100.0, 300.0))];

    let visible_cursor = finish_real_frame(
        &mut harness.controller,
        pointer_on_video.clone(),
        &[],
        false,
        PlaybackState::Playing,
        |_| {},
    );
    assert_ne!(visible_cursor, egui::CursorIcon::None);

    harness.idle_for(DELAY + FRAME);
    let hidden_cursor = finish_real_frame(
        &mut harness.controller,
        pointer_on_video,
        &[],
        false,
        PlaybackState::Playing,
        |_| {},
    );
    assert_eq!(hidden_cursor, egui::CursorIcon::None);
}
