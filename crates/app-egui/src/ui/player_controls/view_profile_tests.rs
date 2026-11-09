//! Нижняя панель в настоящем egui-кадре для обоих видов окна (`WindowViewProfile`).
//!
//! Проверяется то, что увидит человек (кнопки в дереве доступности), а не флаги:
//! обычное окно по-прежнему показывает Open, Previous/Next и Shuffle/Repeat, окно
//! браузерного режима — нет, а play/pause, громкость и фуллскрин есть в обоих.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Event, Modifiers, PointerButton, Pos2, pos2};

use crate::page_quality::PageViewOffer;
use crate::playlist_runtime::{NavigationControlAvailability, PlaylistTransportUiModel};
use crate::state::QualityMenuView;
use crate::ui::player_controls::{
    BottomControlsInput, ControlAction, PageNextEpisodeButton, TransportControlAction,
    render_bottom_controls,
};
use crate::ui::skin::MinimalSkin;
use crate::ui::test_frame::{app_behavior_context, input_at, run_ui_frame};
use crate::ui::timeline::TimelineUiState;
use crate::window_view_profile::WindowViewProfile;

/// Метки кнопок нижней панели без кнопки качества (у неё нет что показать).
fn bottom_panel_button_labels(view_profile: WindowViewProfile) -> BTreeSet<String> {
    bottom_panel_button_labels_with_quality(view_profile, &QualityMenuView::hidden())
}

/// Метки кнопок нижней панели после нескольких кадров (первый кадр только измеряет);
/// `quality_menu` — то, что владелец состояния прислал для кнопки качества.
fn bottom_panel_button_labels_with_quality(
    view_profile: WindowViewProfile,
    quality_menu: &QualityMenuView,
) -> BTreeSet<String> {
    let mut scene = PanelScene::new(
        view_profile,
        quality_menu.clone(),
        PageNextEpisodeButton::NotOffered,
    );
    scene.settle();
    scene.button_centers.keys().cloned().collect()
}

/// Нижняя панель в настоящем egui-кадре: кнопки последнего кадра и их центры.
struct PanelScene {
    context: egui::Context,
    time_seconds: f64,
    view_profile: WindowViewProfile,
    quality_menu: QualityMenuView,
    page_next_episode: PageNextEpisodeButton,
    timeline_state: TimelineUiState,
    button_centers: BTreeMap<String, Pos2>,
}

impl PanelScene {
    fn new(
        view_profile: WindowViewProfile,
        quality_menu: QualityMenuView,
        page_next_episode: PageNextEpisodeButton,
    ) -> Self {
        let context = app_behavior_context();
        context.enable_accesskit();
        Self {
            context,
            time_seconds: 0.0,
            view_profile,
            quality_menu,
            page_next_episode,
            timeline_state: TimelineUiState::default(),
            button_centers: BTreeMap::new(),
        }
    }

    /// Один кадр с событиями; возвращает действия кадра.
    fn frame(&mut self, events: Vec<Event>) -> Vec<ControlAction> {
        self.time_seconds += 0.05;
        let snapshot = player_core::PlayerSnapshot::empty();
        let transport = PlaylistTransportUiModel {
            playlist_view_revision: 1,
            previous: NavigationControlAvailability::Ready,
            next: NavigationControlAvailability::Ready,
            repeat_mode: playlist_core::RepeatMode::StopAtEnd,
            shuffle_enabled: false,
            queue_modes_enabled: true,
            global_status: None,
        };
        let subtitle_menu =
            crate::state::SubtitleMenuView::empty(crate::state::SubtitleVisibility::Shown);
        let audio_menu = crate::state::AudioTrackMenuView::default();
        let mut input = input_at(self.time_seconds, events);
        input.screen_rect = Some(egui::Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2(900.0, 600.0),
        ));
        let mut actions = Vec::new();
        let output = run_ui_frame(&self.context, input, |ui| {
            actions = render_bottom_controls(
                ui,
                BottomControlsInput {
                    player_snapshot: &snapshot,
                    timeline_state: &mut self.timeline_state,
                    timeline_inline_status: None,
                    skin: &MinimalSkin,
                    is_window_fullscreen: false,
                    live_scrub_enabled: false,
                    reduced_motion: true,
                    playlist_transport: &transport,
                    subtitle_menu: &subtitle_menu,
                    audio_menu: &audio_menu,
                    quality_menu: &self.quality_menu,
                    view_profile: self.view_profile,
                    page_next_episode: self.page_next_episode,
                },
            );
        });
        if let Some(update) = output.platform_output.accesskit_update {
            self.button_centers = update
                .nodes
                .iter()
                .filter(|(_, node)| node.role() == egui::accesskit::Role::Button)
                .filter_map(|(_, node)| {
                    let bounds = node.bounds()?;
                    let center = pos2(
                        ((bounds.x0 + bounds.x1) / 2.0) as f32,
                        ((bounds.y0 + bounds.y1) / 2.0) as f32,
                    );
                    node.label().map(|label| (label.to_owned(), center))
                })
                .collect();
        }
        actions
    }

    /// Несколько пустых кадров (первый кадр только измеряет); без ввода действий нет.
    fn settle(&mut self) {
        for _ in 0..3 {
            let actions = self.frame(Vec::new());
            assert!(actions.is_empty(), "no input — no actions: {actions:?}");
        }
    }

    /// Наведение, нажатие и отпускание левой кнопки; возвращает действия отпускания.
    fn left_click(&mut self, position: Pos2) -> Vec<ControlAction> {
        let button_event = |pressed| Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let _hover = self.frame(vec![Event::PointerMoved(position)]);
        let _press = self.frame(vec![button_event(true)]);
        self.frame(vec![button_event(false)])
    }
}

/// Кнопки, которых нет у окна без очереди и без открытия своего медиа.
const QUEUE_AND_OPENING_BUTTONS: [&str; 5] = [
    "Open file",
    "Previous",
    "Next",
    "Shuffle is off. Turn shuffle on",
    "Repeat is off. Repeat Playlist",
];

#[test]
fn the_standard_window_still_shows_open_previous_next_shuffle_and_repeat() {
    let labels = bottom_panel_button_labels(WindowViewProfile::Standard);
    for expected in QUEUE_AND_OPENING_BUTTONS {
        assert!(labels.contains(expected), "{expected} missing: {labels:?}");
    }
}

#[test]
fn the_browser_window_has_no_queue_navigation_and_no_open_button() {
    let browser = bottom_panel_button_labels(WindowViewProfile::BrowserSession);
    for hidden in QUEUE_AND_OPENING_BUTTONS {
        assert!(!browser.contains(hidden), "{hidden} is drawn: {browser:?}");
    }
    // Остальная панель — та же, что у обычного окна: транспорт, громкость, фуллскрин.
    let standard = bottom_panel_button_labels(WindowViewProfile::Standard);
    let common: BTreeSet<String> = standard
        .iter()
        .filter(|label| !QUEUE_AND_OPENING_BUTTONS.contains(&label.as_str()))
        .cloned()
        .collect();
    assert!(
        !common.is_empty(),
        "the panel has common buttons: {standard:?}"
    );
    assert_eq!(browser, common);
}

/// Кнопка качества (подпись «Video quality 720p» для кадра 1280×720): есть у окна браузерного
/// режима, когда есть что показать, и нет ни в обычном окне (даже если ей прислали снимок —
/// профиль главнее), ни в браузерном окне без кадра.
#[test]
fn the_quality_button_exists_only_in_the_browser_window_with_something_to_show() {
    const QUALITY_BUTTON: &str = "Video quality 720p";
    let shown = QualityMenuView::sample_for_tests((1280, 720), PageViewOffer::Available);

    let browser =
        bottom_panel_button_labels_with_quality(WindowViewProfile::BrowserSession, &shown);
    assert!(browser.contains(QUALITY_BUTTON), "{browser:?}");

    let standard = bottom_panel_button_labels_with_quality(WindowViewProfile::Standard, &shown);
    assert!(
        !standard
            .iter()
            .any(|label| label.starts_with("Video quality")),
        "the standard window draws a quality button: {standard:?}"
    );

    let no_frame = bottom_panel_button_labels(WindowViewProfile::BrowserSession);
    assert!(
        !no_frame
            .iter()
            .any(|label| label.starts_with("Video quality")),
        "no frame, no button: {no_frame:?}"
    );
    // Остальные кнопки панели от кнопки качества не меняются.
    let without_quality: BTreeSet<String> = browser
        .into_iter()
        .filter(|label| label != QUALITY_BUTTON)
        .collect();
    assert_eq!(without_quality, no_frame);
}

/// Next окна страницы (14a): виден на своём месте, только когда следующая серия предложена;
/// клик даёт то же действие Next, что ведёт маршрут окна страницы (реклама главнее серии).
#[test]
fn the_browser_window_shows_next_only_when_the_next_episode_is_offered() {
    let mut offered = PanelScene::new(
        WindowViewProfile::BrowserSession,
        QualityMenuView::hidden(),
        PageNextEpisodeButton::Offered,
    );
    offered.settle();
    let next = *offered
        .button_centers
        .get("Next")
        .unwrap_or_else(|| panic!("Next is drawn: {:?}", offered.button_centers.keys()));
    // Только Next: остальная навигация очереди по-прежнему скрыта.
    for hidden in ["Previous", "Shuffle is off. Turn shuffle on", "Open file"] {
        assert!(!offered.button_centers.contains_key(hidden), "{hidden}");
    }
    assert_eq!(
        offered.left_click(next),
        vec![ControlAction::Transport(TransportControlAction::Next)]
    );

    // Не предложена: кнопки нет, и клик по её месту ничего не делает.
    let mut not_offered = PanelScene::new(
        WindowViewProfile::BrowserSession,
        QualityMenuView::hidden(),
        PageNextEpisodeButton::NotOffered,
    );
    not_offered.settle();
    assert!(!not_offered.button_centers.contains_key("Next"));
    assert!(not_offered.left_click(next).is_empty());
}
