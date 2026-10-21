//! UI-модель кадра и видимость позиции (cpu-parity, сессия 06b): скрытая автоскрытием нижняя
//! панель не должна делать модель «изменившейся» от одного хода времени.
//!
//! Модель собирается настоящим [`UiFrameModel::build`], а видимость панелей даёт настоящий
//! [`FullscreenChromeController`] — тот же, что рисует кадр.

use std::time::{Duration, Instant};

use media_core::{MediaDuration, MediaTime, TimelineMode, TimelinePreviewState, TimelineSnapshot};
use player_core::{PlaybackRate, PlaybackState, PlayerSnapshot};
use playlist_core::RepeatMode;

use super::*;
use crate::fullscreen_chrome::{
    AutohideDelay, ChromeFrameInput, FullscreenChromeController, UserActivity,
    WindowPresentationMode,
};
use crate::playlist_runtime::{
    NavigationControlAvailability, PlaylistInteractionModel, PlaylistTransportUiModel,
    PlaylistUndoUiSnapshot,
};
use crate::state::notifications::BufferingIndicator;
use crate::ui::animation::UiMotion;

/// Задержка автоскрытия, как в настройках по умолчанию.
const AUTOHIDE_DELAY: Duration = Duration::from_millis(2500);
/// 128 с на 1024 px: пиксель ползунка = 125 мс.
const TRACK_WIDTH_PX: Option<u32> = Some(1024);

/// Играющий snapshot 128-секундного файла в позиции `position_ms`.
fn playing_snapshot(position_ms: u64) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.playback_state = PlaybackState::Playing;
    snapshot.playback_rate = PlaybackRate::new(1.0).expect("valid rate");
    snapshot.current_position = Duration::from_millis(position_ms);
    snapshot.timeline = TimelineSnapshot {
        mode: TimelineMode::default(),
        current_position: MediaTime::from_millis(position_ms),
        target_position: None,
        duration: Some(MediaDuration::from_duration(Duration::from_secs(128))),
        live_edge: None,
        live_epoch: None,
        live_revision: None,
        seekable_range: None,
        seekable: true,
        not_seekable_reason: None,
        seeking: false,
        scrubbing: false,
        stale_frame: false,
        preview_state: TimelinePreviewState::Inactive,
    };
    snapshot
}

/// Пустой плейлист: в этих тестах он не меняется и в сравнение не вмешивается.
fn idle_playlist() -> PlaylistVisualKey {
    PlaylistVisualKey {
        view_revision: None,
        interaction: PlaylistInteractionModel::default(),
        transport: PlaylistTransportUiModel {
            playlist_view_revision: 0,
            previous: NavigationControlAvailability::Disabled,
            next: NavigationControlAvailability::Disabled,
            repeat_mode: RepeatMode::StopAtEnd,
            shuffle_enabled: false,
            queue_modes_enabled: false,
            global_status: None,
        },
        undo: PlaylistUndoUiSnapshot {
            undo: None,
            next_wake_deadline: None,
        },
        import_preview: None,
        confirmation: None,
    }
}

/// Кадр без уведомлений.
fn quiet_notifications() -> NotificationsFrame {
    NotificationsFrame {
        center: None,
        toasts: Vec::new(),
        buffering: BufferingIndicator::Hidden,
        motion: UiMotion::Standard,
    }
}

/// Модель кадра для snapshot-а при текущем состоянии автоскрытия; остальное неизменно.
fn model_for(snapshot: &PlayerSnapshot, chrome: &FullscreenChromeController) -> UiFrameModel {
    let notifications = quiet_notifications();
    let timeline_ui_state = TimelineUiState::default();
    UiFrameModel::build(
        PlayerVisualFacts {
            snapshot,
            timeline_track_width_px: TRACK_WIDTH_PX,
        },
        AppVisualFacts {
            window: WindowFacts {
                inner_size: [1920, 1080],
                pixels_per_point_bits: 1.0_f32.to_bits(),
                is_maximized: false,
                is_fullscreen: true,
            },
            timeline_ui_state: &timeline_ui_state,
            timeline_inline_status: None,
            startup_pending: false,
            chrome: chrome.presentation(),
            sidebar: SidebarVisualKey {
                displayed: None,
                target: None,
                open_progress: 0.0,
                content_transition: None,
                open_width_points: 320.0,
            },
            notifications: &notifications,
            toast_fade_in: Duration::from_millis(150),
            telemetry_open: false,
            settings_window_open: false,
            settings_ui_revision: 0,
            reduced_motion: true,
            playlist: idle_playlist(),
        },
    )
}

/// Контроллер автоскрытия на управляемом времени; анимация мгновенная (reduced motion),
/// чтобы показ/скрытие занимали ровно один кадр.
struct Chrome {
    controller: FullscreenChromeController,
    now: Instant,
}

impl Chrome {
    fn new() -> Self {
        Self {
            controller: FullscreenChromeController::default(),
            now: Instant::now(),
        }
    }

    fn frame(&mut self, mode: WindowPresentationMode, step: Duration, activity: UserActivity) {
        self.now += step;
        self.controller.advance(ChromeFrameInput {
            mode,
            activity,
            delay: AutohideDelay::After(AUTOHIDE_DELAY),
            slide_duration_seconds: 0.0,
            now: self.now,
        });
    }

    /// Фуллскрин без ввода дольше задержки: панели уехали.
    fn hidden_in_fullscreen() -> Self {
        let mut chrome = Self::new();
        chrome.frame(
            WindowPresentationMode::Fullscreen,
            Duration::ZERO,
            UserActivity::Detected,
        );
        chrome.frame(
            WindowPresentationMode::Fullscreen,
            AUTOHIDE_DELAY,
            UserActivity::Idle,
        );
        chrome
    }
}

/// Скрытая панель + идущее время: модель не меняется — полного прохода ради позиции нет.
#[test]
fn hidden_panel_keeps_the_model_still_while_the_position_runs() {
    let chrome = Chrome::hidden_in_fullscreen();
    let baseline = model_for(&playing_snapshot(12_000), &chrome.controller);
    assert_eq!(baseline.position, VisibleTimelinePosition::OffScreen);

    // Новый пиксель, новая секунда, далёкая позиция — для скрытой панели всё одно.
    for position_ms in [12_125, 13_000, 60_000] {
        let later = model_for(&playing_snapshot(position_ms), &chrome.controller);
        assert!(
            later == baseline,
            "hidden panel: position {position_ms} ms must not change the model"
        );
    }
}

/// Видимая панель (окно): модель по-прежнему следит за пикселем и секундой (сессия 04).
#[test]
fn visible_panel_model_follows_pixel_and_second_as_before() {
    let mut chrome = Chrome::new();
    chrome.frame(
        WindowPresentationMode::Windowed,
        Duration::ZERO,
        UserActivity::Idle,
    );
    let at_pixel = model_for(&playing_snapshot(12_000), &chrome.controller);
    let same_pixel = model_for(&playing_snapshot(12_100), &chrome.controller);
    let next_pixel = model_for(&playing_snapshot(12_125), &chrome.controller);

    assert!(
        same_pixel == at_pixel,
        "inside one pixel and second nothing changes"
    );
    assert!(
        next_pixel != at_pixel,
        "a new slider pixel is a visible change"
    );
    assert_eq!(
        next_pixel.position,
        VisibleTimelinePosition::Shown(quantize_timeline_position(
            &playing_snapshot(12_125).timeline,
            TRACK_WIDTH_PX
        ))
    );
}

/// Показ панели движением мыши: модель меняется один раз и несёт актуальную позицию,
/// дальше без хода времени — стабильна (ровно один полный проход на показ).
#[test]
fn showing_the_panel_changes_the_model_once_with_the_actual_position() {
    let mut chrome = Chrome::hidden_in_fullscreen();
    let snapshot = playing_snapshot(30_000);
    let hidden = model_for(&snapshot, &chrome.controller);
    assert_eq!(hidden.position, VisibleTimelinePosition::OffScreen);

    chrome.frame(
        WindowPresentationMode::Fullscreen,
        Duration::from_millis(16),
        UserActivity::Detected,
    );
    let shown = model_for(&snapshot, &chrome.controller);
    assert!(shown != hidden, "showing the panel is a visible change");
    assert_eq!(
        shown.position,
        VisibleTimelinePosition::Shown(quantize_timeline_position(
            &snapshot.timeline,
            TRACK_WIDTH_PX
        )),
        "the first frame of the shown panel carries the actual position"
    );

    chrome.frame(
        WindowPresentationMode::Fullscreen,
        Duration::from_millis(16),
        UserActivity::Idle,
    );
    let next_frame = model_for(&snapshot, &chrome.controller);
    assert!(
        next_frame == shown,
        "nothing changes on the next frame: no second pass"
    );
}
