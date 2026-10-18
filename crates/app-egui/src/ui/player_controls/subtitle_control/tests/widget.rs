//! Настоящий egui-кадр: клики по CC и стрелке, меню, Esc, доступность и анимация появления.

use std::collections::BTreeMap;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, pos2};
use subtitle_core::{SubtitleSyncOffset, SubtitleTrackId};

use super::super::layout::{SubtitleControlMode, choose_mode, layout};
use super::super::{SubtitleControlAction, SubtitleControlFrame};
use super::{geometry_for_window_width, menu_track, menu_view};
use crate::state::{
    SubtitleMenuView, SubtitleTrackAvailability, SubtitleUnavailableReason, SubtitleVisibility,
};
use crate::ui::player_controls::ControlAction;
use crate::ui::test_frame::{app_behavior_context, input_at, run_ui_frame};

/// Окно, в котором помещается полный режим (CC и стрелка).
const FULL_WINDOW: f32 = 800.0;
/// Окно, в котором помещается только компактный режим.
const COMPACT_WINDOW: f32 = 480.0;

fn subtitles(action: SubtitleControlAction) -> ControlAction {
    ControlAction::Subtitles(action)
}

/// Контекст egui, время кадров и сборка действий одного сценария.
struct Scene {
    context: egui::Context,
    time_seconds: f64,
    window_width: f32,
    reduced_motion: bool,
    /// Метки узлов AccessKit последнего кадра -> центр узла.
    node_centers: BTreeMap<String, Pos2>,
    /// Узлы AccessKit последнего кадра: метка -> (toggled, роль).
    toggled_by_label: BTreeMap<String, Option<egui::accesskit::Toggled>>,
}

impl Scene {
    fn new(window_width: f32) -> Self {
        let context = egui::Context::default();
        context.enable_accesskit();
        Self {
            context,
            time_seconds: 0.0,
            window_width,
            reduced_motion: true,
            node_centers: BTreeMap::new(),
            toggled_by_label: BTreeMap::new(),
        }
    }

    /// Прогоняет один кадр с событиями и возвращает действия кадра.
    fn frame(&mut self, events: Vec<Event>, view: &SubtitleMenuView) -> Vec<ControlAction> {
        self.time_seconds += 0.05;
        let geometry = geometry_for_window_width(self.window_width);
        let reduced_motion = self.reduced_motion;
        let mut actions = Vec::new();
        let output = run_ui_frame(&self.context, input_at(self.time_seconds, events), |ui| {
            let frame = SubtitleControlFrame::prepare(ui, geometry, view, reduced_motion);
            frame.render(ui, view, reduced_motion, &mut actions);
        });
        if let Some(update) = output.platform_output.accesskit_update {
            self.node_centers.clear();
            self.toggled_by_label.clear();
            for (_, node) in &update.nodes {
                // Подписи кнопок лежат в `label`, текст Label — в `value`.
                let Some(label) = node.label().or_else(|| node.value()) else {
                    continue;
                };
                if let Some(bounds) = node.bounds() {
                    self.node_centers.insert(
                        label.to_owned(),
                        pos2(
                            ((bounds.x0 + bounds.x1) / 2.0) as f32,
                            ((bounds.y0 + bounds.y1) / 2.0) as f32,
                        ),
                    );
                }
                self.toggled_by_label
                    .insert(label.to_owned(), node.toggled());
            }
        }
        actions
    }

    /// Несколько пустых кадров: попап проходит свой «измерительный» кадр.
    fn settle(&mut self, view: &SubtitleMenuView) {
        for _ in 0..3 {
            let _ = self.frame(Vec::new(), view);
        }
    }

    /// Наведение, нажатие и отпускание кнопки мыши в точке; возвращает действия отпускания.
    fn click(
        &mut self,
        position: Pos2,
        button: PointerButton,
        view: &SubtitleMenuView,
    ) -> Vec<ControlAction> {
        let event = |pressed| Event::PointerButton {
            pos: position,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let hover_actions = self.frame(vec![Event::PointerMoved(position)], view);
        let press_actions = self.frame(vec![event(true)], view);
        assert!(hover_actions.is_empty() && press_actions.is_empty());
        self.frame(vec![event(false)], view)
    }

    fn left_click(&mut self, position: Pos2, view: &SubtitleMenuView) -> Vec<ControlAction> {
        self.click(position, PointerButton::Primary, view)
    }

    fn center_of(&self, label: &str) -> Pos2 {
        *self.node_centers.get(label).unwrap_or_else(|| {
            panic!(
                "no AccessKit node `{label}` in {:?}",
                self.node_centers.keys()
            )
        })
    }

    fn popup_open(&self) -> bool {
        egui::Popup::is_any_open(&self.context)
    }
}

/// Позиции CC и стрелки для окна `window_width` при полном появлении.
fn button_centers(window_width: f32) -> (Pos2, Option<Pos2>) {
    let geometry = geometry_for_window_width(window_width);
    let result = layout(&geometry, choose_mode(&geometry), 1.0);
    (
        result.button_rect.expect("CC is visible").center(),
        result.arrow_rect.map(|rect| rect.center()),
    )
}

fn two_track_view(visibility: SubtitleVisibility, offset_millis: i64) -> SubtitleMenuView {
    menu_view(
        visibility,
        vec![
            menu_track(1, Some("ru"), true, SubtitleTrackAvailability::Available),
            menu_track(2, Some("en"), false, SubtitleTrackAvailability::Available),
            menu_track(3, None, false, SubtitleTrackAvailability::Available),
        ],
        SubtitleSyncOffset::from_millis(offset_millis),
    )
}

#[test]
fn clicking_cc_emits_toggle_and_leaves_no_keyboard_focus() {
    let view = two_track_view(SubtitleVisibility::Shown, 0);
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    let (cc_center, _) = button_centers(FULL_WINDOW);

    let actions = scene.left_click(cc_center, &view);

    assert_eq!(
        actions,
        vec![subtitles(SubtitleControlAction::ToggleVisibility)]
    );
    assert!(
        !scene.popup_open(),
        "CC alone never opens the menu in full mode"
    );
    assert!(
        scene.context.memory(|memory| memory.focused()).is_none(),
        "pointer click must not leave a keyboard outline"
    );
}

#[test]
fn accessibility_selected_state_follows_visibility_and_arrow_is_labelled() {
    let mut scene = Scene::new(FULL_WINDOW);
    let shown = two_track_view(SubtitleVisibility::Shown, 0);
    scene.settle(&shown);
    assert_eq!(
        scene.toggled_by_label.get("Subtitles"),
        Some(&Some(egui::accesskit::Toggled::True))
    );
    assert!(scene.toggled_by_label.contains_key("Subtitle options"));

    let hidden = two_track_view(SubtitleVisibility::Hidden, 0);
    scene.settle(&hidden);
    assert_eq!(
        scene.toggled_by_label.get("Subtitles"),
        Some(&Some(egui::accesskit::Toggled::False))
    );
}

#[test]
fn no_tracks_means_no_widget_at_all_and_clicks_do_nothing() {
    let view = menu_view(
        SubtitleVisibility::Shown,
        Vec::new(),
        SubtitleSyncOffset::ZERO,
    );
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    assert!(!scene.toggled_by_label.contains_key("Subtitles"));
    assert!(!scene.toggled_by_label.contains_key("Subtitle options"));

    let (cc_center, arrow_center) = button_centers(FULL_WINDOW);
    assert!(scene.left_click(cc_center, &view).is_empty());
    assert!(
        scene
            .left_click(arrow_center.expect("full mode"), &view)
            .is_empty()
    );
    assert!(!scene.popup_open());
}

#[test]
fn too_narrow_window_hides_the_button_but_not_the_ability_to_use_hotkeys() {
    let view = two_track_view(SubtitleVisibility::Shown, 0);
    let mut scene = Scene::new(420.0);
    scene.settle(&view);
    assert!(
        !scene.toggled_by_label.contains_key("Subtitles"),
        "hidden mode draws no CC"
    );
}

#[test]
fn arrow_opens_the_menu_and_every_item_emits_its_exact_action() {
    let view = two_track_view(SubtitleVisibility::Shown, 300);
    let (_, arrow_center) = button_centers(FULL_WINDOW);
    let arrow_center = arrow_center.expect("full mode has an arrow");

    let cases = [
        (
            "English (SRT, file)",
            subtitles(SubtitleControlAction::SelectTrack(SubtitleTrackId::new(2))),
        ),
        (
            "Track 3 (SRT, file)",
            subtitles(SubtitleControlAction::SelectTrack(SubtitleTrackId::new(3))),
        ),
        ("Off", subtitles(SubtitleControlAction::Disable)),
        (
            "Load file…",
            subtitles(SubtitleControlAction::OpenLoadFileDialog),
        ),
        ("Reset", subtitles(SubtitleControlAction::ResetSync)),
    ];
    for (label, expected) in cases {
        let mut scene = Scene::new(FULL_WINDOW);
        scene.settle(&view);
        assert!(scene.left_click(arrow_center, &view).is_empty());
        scene.settle(&view);
        assert!(scene.popup_open(), "arrow click opens the menu for {label}");

        let actions = scene.left_click(scene.center_of(label), &view);
        assert_eq!(actions, vec![expected], "menu item `{label}`");
        scene.settle(&view);
        assert!(!scene.popup_open(), "menu closes after `{label}`");
    }
}

#[test]
fn menu_lists_tracks_off_and_the_current_delay_in_order() {
    let view = two_track_view(SubtitleVisibility::Shown, -200);
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    let (_, arrow_center) = button_centers(FULL_WINDOW);
    scene.left_click(arrow_center.expect("arrow"), &view);
    scene.settle(&view);

    let y_of = |label: &str| scene.center_of(label).y;
    let ordered = [
        "Russian (SRT, file)",
        "English (SRT, file)",
        "Track 3 (SRT, file)",
        "Off",
        "Load file…",
    ];
    for pair in ordered.windows(2) {
        assert!(
            y_of(pair[0]) < y_of(pair[1]),
            "{} above {}",
            pair[0],
            pair[1]
        );
    }
    assert!(scene.node_centers.contains_key("Delay: -0.2 s"));
    assert_eq!(
        scene.toggled_by_label.get("Russian (SRT, file)"),
        Some(&Some(egui::accesskit::Toggled::True)),
        "the selected track is marked"
    );
    assert_eq!(
        scene.toggled_by_label.get("Off"),
        Some(&Some(egui::accesskit::Toggled::False)),
        "Off is not marked while subtitles are shown"
    );
}

#[test]
fn off_is_marked_when_subtitles_are_hidden() {
    let view = two_track_view(SubtitleVisibility::Hidden, 0);
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    let (_, arrow_center) = button_centers(FULL_WINDOW);
    scene.left_click(arrow_center.expect("arrow"), &view);
    scene.settle(&view);
    assert_eq!(
        scene.toggled_by_label.get("Off"),
        Some(&Some(egui::accesskit::Toggled::True))
    );
}

#[test]
fn unavailable_track_and_reset_at_zero_emit_nothing_and_keep_the_menu_open() {
    let view = menu_view(
        SubtitleVisibility::Shown,
        vec![
            menu_track(1, Some("en"), true, SubtitleTrackAvailability::Available),
            menu_track(
                2,
                Some("ja"),
                false,
                SubtitleTrackAvailability::Unavailable(
                    SubtitleUnavailableReason::FormatNotSupportedYet,
                ),
            ),
        ],
        SubtitleSyncOffset::ZERO,
    );
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    let (_, arrow_center) = button_centers(FULL_WINDOW);
    scene.left_click(arrow_center.expect("arrow"), &view);
    scene.settle(&view);

    assert!(
        scene
            .left_click(scene.center_of("Japanese (SRT, file)"), &view)
            .is_empty()
    );
    scene.settle(&view);
    assert!(
        scene.popup_open(),
        "a disabled item does not close the menu"
    );
    assert!(scene.left_click(scene.center_of("Reset"), &view).is_empty());
    scene.settle(&view);
    assert!(scene.popup_open());
}

#[test]
fn escape_closes_the_open_menu() {
    let view = two_track_view(SubtitleVisibility::Shown, 0);
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    let (_, arrow_center) = button_centers(FULL_WINDOW);
    scene.left_click(arrow_center.expect("arrow"), &view);
    scene.settle(&view);
    assert!(scene.popup_open());

    let escape = |pressed| Event::Key {
        key: Key::Escape,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    let _ = scene.frame(vec![escape(true), escape(false)], &view);
    scene.settle(&view);
    assert!(!scene.popup_open());
}

#[test]
fn click_outside_the_menu_closes_it_without_actions() {
    let view = two_track_view(SubtitleVisibility::Shown, 0);
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    let (_, arrow_center) = button_centers(FULL_WINDOW);
    scene.left_click(arrow_center.expect("arrow"), &view);
    scene.settle(&view);

    let actions = scene.left_click(pos2(20.0, 400.0), &view);
    assert!(actions.is_empty());
    scene.settle(&view);
    assert!(!scene.popup_open());
}

#[test]
fn compact_mode_left_click_toggles_and_right_click_opens_the_menu() {
    let geometry = geometry_for_window_width(COMPACT_WINDOW);
    assert_eq!(choose_mode(&geometry), SubtitleControlMode::Compact);
    let view = two_track_view(SubtitleVisibility::Shown, 0);
    let mut scene = Scene::new(COMPACT_WINDOW);
    scene.settle(&view);
    assert!(
        !scene.toggled_by_label.contains_key("Subtitle options"),
        "compact mode has no separate arrow"
    );
    let (cc_center, arrow_center) = button_centers(COMPACT_WINDOW);
    assert_eq!(arrow_center, None);

    let left = scene.left_click(cc_center, &view);
    assert_eq!(
        left,
        vec![subtitles(SubtitleControlAction::ToggleVisibility)]
    );
    assert!(!scene.popup_open(), "left click never opens the menu");

    let right = scene.click(cc_center, PointerButton::Secondary, &view);
    assert!(right.is_empty(), "right click only opens the menu");
    scene.settle(&view);
    assert!(scene.popup_open());

    let actions = scene.left_click(scene.center_of("Off"), &view);
    assert_eq!(actions, vec![subtitles(SubtitleControlAction::Disable)]);
}

#[test]
fn reduced_motion_shows_cc_instantly_and_without_repaint() {
    let view = two_track_view(SubtitleVisibility::Shown, 0);
    let mut scene = Scene::new(FULL_WINDOW);
    scene.reduced_motion = true;
    let _ = scene.frame(Vec::new(), &view);
    assert!(
        scene.toggled_by_label.contains_key("Subtitles"),
        "CC is there on the very first frame"
    );
}

#[test]
fn reveal_animation_runs_for_250_ms_and_then_stops_requesting_repaints() {
    let context = app_behavior_context();
    let geometry = geometry_for_window_width(FULL_WINDOW);
    let view = two_track_view(SubtitleVisibility::Shown, 0);
    let no_tracks = menu_view(
        SubtitleVisibility::Shown,
        Vec::new(),
        SubtitleSyncOffset::ZERO,
    );

    let progress_at = |time_seconds: f64, view: &SubtitleMenuView| {
        let mut progress = f32::NAN;
        let mut actions = Vec::new();
        let _ = run_ui_frame(&context, input_at(time_seconds, Vec::new()), |ui| {
            let frame = SubtitleControlFrame::prepare(ui, geometry, view, false);
            progress = frame.reveal_progress;
            frame.render(ui, view, false, &mut actions);
        });
        progress
    };

    // Сначала дорожек нет, потом они появляются.
    assert_eq!(progress_at(0.0, &no_tracks), 0.0);
    assert_eq!(progress_at(0.1, &no_tracks), 0.0);
    let started = progress_at(0.2, &view);
    let halfway = progress_at(0.325, &view);
    let finished = progress_at(0.45, &view);
    let settled = progress_at(0.6, &view);
    assert!(started < 0.5, "starts near zero, got {started}");
    assert!(
        halfway > started && halfway < 1.0,
        "mid animation, got {halfway}"
    );
    assert_eq!(finished, 1.0, "250 ms later the button is fully there");
    assert_eq!(settled, 1.0);
    // egui отдаёт ещё один «закрывающий» repaint после достижения цели; дальше тишина.
    assert_eq!(progress_at(0.8, &view), 1.0);
    assert_eq!(progress_at(0.9, &view), 1.0);
    assert!(
        !context.requested_repaint_last_pass(),
        "a settled animation must not keep repainting"
    );
}

/// Строка меню web-дорожки заданного вида.
fn web_menu_track(
    raw_id: u64,
    kind: web_media_core::SubtitleKind,
    language: &str,
    is_selected: bool,
) -> crate::state::SubtitleMenuTrack {
    let mut track = menu_track(
        raw_id,
        Some(language),
        is_selected,
        SubtitleTrackAvailability::Available,
    );
    track.label.format = None;
    track.label.origin = crate::state::SubtitleFileOrigin::Web(kind);
    track
}

#[test]
fn auto_translations_live_in_a_submenu_and_a_click_inside_it_selects_and_closes_everything() {
    use web_media_core::SubtitleKind::{AutoGenerated, AutoTranslated, Manual};
    let view = menu_view(
        SubtitleVisibility::Shown,
        vec![
            web_menu_track(1, Manual, "en", true),
            web_menu_track(2, AutoGenerated, "en", false),
            web_menu_track(3, AutoTranslated, "ru", false),
            web_menu_track(4, AutoTranslated, "de", false),
        ],
        SubtitleSyncOffset::ZERO,
    );
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    let (_, arrow_center) = button_centers(FULL_WINDOW);
    scene.left_click(arrow_center.expect("arrow"), &view);
    scene.settle(&view);

    for label in ["English", "English (auto)", "Auto-translate ⏵"] {
        assert!(
            scene.node_centers.contains_key(label),
            "main menu shows `{label}`: {:?}",
            scene.node_centers.keys()
        );
    }
    assert!(
        !scene
            .node_centers
            .contains_key("Russian (auto, translated)"),
        "translations are hidden until the submenu opens"
    );

    scene.left_click(scene.center_of("Auto-translate ⏵"), &view);
    scene.settle(&view);
    assert!(
        scene.center_of("Russian (auto, translated)").y
            < scene.center_of("German (auto, translated)").y,
        "the submenu keeps the order given by the owner"
    );

    let actions = scene.left_click(scene.center_of("German (auto, translated)"), &view);
    assert_eq!(
        actions,
        vec![subtitles(SubtitleControlAction::SelectTrack(
            SubtitleTrackId::new(4)
        ))]
    );
    scene.settle(&view);
    assert!(
        !scene.popup_open(),
        "choosing a translation closes the whole menu"
    );
}

#[test]
fn menu_without_translations_has_no_submenu_entry() {
    use web_media_core::SubtitleKind::Manual;
    let view = menu_view(
        SubtitleVisibility::Shown,
        vec![web_menu_track(1, Manual, "en", true)],
        SubtitleSyncOffset::ZERO,
    );
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    let (_, arrow_center) = button_centers(FULL_WINDOW);
    scene.left_click(arrow_center.expect("arrow"), &view);
    scene.settle(&view);

    assert!(scene.node_centers.contains_key("English"));
    assert!(!scene.node_centers.contains_key("Auto-translate ⏵"));
}
