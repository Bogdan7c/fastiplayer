//! Настоящий egui-кадр: клики по «♪» и стрелке, меню, выбор дорожки и отсутствие виджета.

use std::collections::BTreeMap;

use egui::{Event, Modifiers, PointerButton, Pos2, pos2};
use media_core::TrackId;

use super::super::layout::{choose_mode, layout};
use super::super::{AudioTrackControlAction, AudioTrackControlFrame};
use super::neighbor_for_width;
use crate::state::AudioTrackMenuView;
use crate::ui::player_controls::ControlAction;
use crate::ui::test_frame::{app_behavior_context, input_at, run_ui_frame};

/// Окно, в котором «♪» показывается полностью (со стрелкой) рядом с CC.
const FULL_WINDOW: f32 = 800.0;
/// Окно, в котором «♪» не помещается (CC при этом есть).
const NARROW_WINDOW: f32 = 520.0;

/// Сцена: контекст egui, время кадров и узлы AccessKit последнего кадра.
struct Scene {
    context: egui::Context,
    time_seconds: f64,
    window_width: f32,
    node_centers: BTreeMap<String, Pos2>,
}

impl Scene {
    fn new(window_width: f32) -> Self {
        let context = app_behavior_context();
        context.enable_accesskit();
        Self {
            context,
            time_seconds: 0.0,
            window_width,
            node_centers: BTreeMap::new(),
        }
    }

    /// Один кадр с событиями; возвращает действия кадра.
    fn frame(&mut self, events: Vec<Event>, view: &AudioTrackMenuView) -> Vec<ControlAction> {
        self.time_seconds += 0.05;
        let neighbor = neighbor_for_width(self.window_width, true);
        let mut actions = Vec::new();
        let output = run_ui_frame(&self.context, input_at(self.time_seconds, events), |ui| {
            let frame = AudioTrackControlFrame::prepare(ui, neighbor, view, true);
            frame.render(ui, view, true, &mut actions);
        });
        if let Some(update) = output.platform_output.accesskit_update {
            self.node_centers.clear();
            for (_, node) in &update.nodes {
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
            }
        }
        actions
    }

    /// Несколько пустых кадров: попап проходит свой «измерительный» кадр.
    fn settle(&mut self, view: &AudioTrackMenuView) {
        for _ in 0..3 {
            let _ = self.frame(Vec::new(), view);
        }
    }

    /// Наведение, нажатие и отпускание левой кнопки; возвращает действия отпускания.
    fn left_click(&mut self, position: Pos2, view: &AudioTrackMenuView) -> Vec<ControlAction> {
        let button_event = |pressed| Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let hover = self.frame(vec![Event::PointerMoved(position)], view);
        let press = self.frame(vec![button_event(true)], view);
        assert!(hover.is_empty() && press.is_empty());
        self.frame(vec![button_event(false)], view)
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

/// Центры «♪» и стрелки при полном появлении в окне `window_width`.
fn centers(window_width: f32) -> (Pos2, Pos2) {
    let neighbor = neighbor_for_width(window_width, true);
    let result = layout(&neighbor, choose_mode(&neighbor), 1.0);
    (
        result.button_rect.expect("audio visible").center(),
        result.arrow_rect.expect("full mode has an arrow").center(),
    )
}

/// Две дорожки как у `two-audio-tracks.mkv`: выбрана первая.
fn two_track_view() -> AudioTrackMenuView {
    use media_core::{TrackKind, TrackLabels, TrackLanguage};
    let mut snapshot = player_core::PlayerSnapshot::empty();
    let summary =
        |raw_id: u32, codec: &str, language: &str, title: &str| player_core::TrackSummarySnapshot {
            id: TrackId::new(raw_id),
            kind: TrackKind::Audio,
            codec_id: codec.to_owned(),
            sample_rate: None,
            channels: None,
            duration: None,
            video: None,
            video_color_summary: None,
            labels: TrackLabels {
                language: Some(TrackLanguage::new(language)),
                title: Some(title.to_owned()),
                disposition: media_core::TrackDisposition::default(),
            },
        };
    snapshot.tracks = vec![
        summary(2, "A_FLAC", "rus", "Dub"),
        summary(3, "A_OPUS", "eng", "Original"),
    ];
    snapshot.selected_tracks.audio_track = Some(TrackId::new(2));
    AudioTrackMenuView::from_snapshot(&snapshot)
}

/// Одна аудиодорожка: выбирать не из чего.
fn one_track_view() -> AudioTrackMenuView {
    let two = two_track_view();
    let mut snapshot = player_core::PlayerSnapshot::empty();
    snapshot.tracks = vec![player_core::TrackSummarySnapshot {
        id: match two.tracks()[0].target {
            crate::state::AudioTrackMenuTarget::EmbeddedTrack(track_id) => track_id,
            crate::state::AudioTrackMenuTarget::WebAudioVersion(_)
            | crate::state::AudioTrackMenuTarget::WebAudioLanguage(_) => {
                panic!("two_track_view contains only embedded tracks")
            }
        },
        kind: media_core::TrackKind::Audio,
        codec_id: "A_FLAC".to_owned(),
        sample_rate: None,
        channels: None,
        duration: None,
        video: None,
        video_color_summary: None,
        labels: media_core::TrackLabels::default(),
    }];
    AudioTrackMenuView::from_snapshot(&snapshot)
}

fn select(raw_id: u32) -> ControlAction {
    ControlAction::AudioTracks(AudioTrackControlAction::SelectTrack(TrackId::new(raw_id)))
}

#[test]
fn clicking_the_note_or_the_arrow_opens_the_menu_without_any_action() {
    let view = two_track_view();
    let (note, arrow) = centers(FULL_WINDOW);
    for (name, position) in [("note", note), ("arrow", arrow)] {
        let mut scene = Scene::new(FULL_WINDOW);
        scene.settle(&view);
        assert!(scene.left_click(position, &view).is_empty(), "{name}");
        scene.settle(&view);
        assert!(scene.popup_open(), "{name} click opens the menu");
        assert!(
            scene.context.memory(|memory| memory.focused()).is_none(),
            "pointer click must not leave a keyboard outline"
        );
    }
}

#[test]
fn menu_lists_both_tracks_and_clicking_one_selects_exactly_that_track() {
    let view = two_track_view();
    let (note, _) = centers(FULL_WINDOW);
    for (label, expected) in [
        ("English — Original (Opus)", select(3)),
        // Выбранная дорожка тоже кликабельна: воронка сама отсечёт повтор.
        ("Russian — Dub (FLAC)", select(2)),
    ] {
        let mut scene = Scene::new(FULL_WINDOW);
        scene.settle(&view);
        scene.left_click(note, &view);
        scene.settle(&view);

        let actions = scene.left_click(scene.center_of(label), &view);
        assert_eq!(actions, vec![expected], "menu item `{label}`");
        scene.settle(&view);
        assert!(!scene.popup_open(), "menu closes after `{label}`");
    }
}

#[test]
fn one_audio_track_means_no_widget_and_clicks_do_nothing() {
    let view = one_track_view();
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    assert!(!scene.node_centers.contains_key("Audio tracks"));

    let (note, arrow) = centers(FULL_WINDOW);
    assert!(scene.left_click(note, &view).is_empty());
    assert!(scene.left_click(arrow, &view).is_empty());
    assert!(!scene.popup_open());
}

#[test]
fn too_narrow_window_hides_the_button() {
    let view = two_track_view();
    let mut scene = Scene::new(NARROW_WINDOW);
    scene.settle(&view);
    assert!(!scene.node_centers.contains_key("Audio tracks"));
}

#[test]
fn visible_button_is_labelled_for_accessibility() {
    let view = two_track_view();
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    assert!(scene.node_centers.contains_key("Audio tracks"));
}

/// Нижняя панель целиком: «♪» появляется у media с двумя дорожками и стоит левее Fullscreen.
#[test]
fn bottom_panel_shows_the_audio_button_only_for_two_or_more_tracks() {
    use crate::playlist_runtime::{NavigationControlAvailability, PlaylistTransportUiModel};
    use crate::ui::player_controls::{BottomControlsInput, render_bottom_controls};
    use crate::ui::skin::MinimalSkin;
    use crate::ui::timeline::TimelineUiState;

    let panel_labels = |view: &AudioTrackMenuView| {
        let context = app_behavior_context();
        context.enable_accesskit();
        let snapshot = player_core::PlayerSnapshot::empty();
        let transport = PlaylistTransportUiModel {
            playlist_view_revision: 1,
            previous: NavigationControlAvailability::Disabled,
            next: NavigationControlAvailability::Disabled,
            repeat_mode: playlist_core::RepeatMode::StopAtEnd,
            shuffle_enabled: false,
            queue_modes_enabled: true,
            global_status: None,
        };
        let subtitle_menu =
            crate::state::SubtitleMenuView::empty(crate::state::SubtitleVisibility::Shown);
        let mut timeline_state = TimelineUiState::default();
        let mut labels = BTreeMap::new();
        for frame_index in 0..3 {
            let mut input = input_at(0.05 * f64::from(frame_index), Vec::new());
            input.screen_rect = Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(FULL_WINDOW, 600.0),
            ));
            let output = run_ui_frame(&context, input, |ui| {
                let _ = render_bottom_controls(
                    ui,
                    BottomControlsInput {
                        player_snapshot: &snapshot,
                        timeline_state: &mut timeline_state,
                        timeline_inline_status: None,
                        skin: &MinimalSkin,
                        is_window_fullscreen: false,
                        live_scrub_enabled: false,
                        reduced_motion: true,
                        playlist_transport: &transport,
                        subtitle_menu: &subtitle_menu,
                        audio_menu: view,
                    },
                );
            });
            if let Some(update) = output.platform_output.accesskit_update {
                labels = update
                    .nodes
                    .iter()
                    .filter_map(|(_, node)| Some((node.label()?.to_owned(), node.bounds()?)))
                    .collect();
            }
        }
        labels
    };

    let with_two = panel_labels(&two_track_view());
    let note = with_two
        .get("Audio tracks")
        .expect("two tracks show the button");
    let fullscreen = with_two
        .iter()
        .find(|(label, _)| label.to_lowercase().contains("fullscreen"))
        .map(|(_, bounds)| *bounds)
        .expect("fullscreen button");
    assert!(note.x1 < fullscreen.x0, "audio is left of Fullscreen");

    assert!(!panel_labels(&one_track_view()).contains_key("Audio tracks"));
}

#[test]
fn clicking_a_language_row_publishes_the_language_switch_with_its_action() {
    use media_source_open::web_media_open::audio_languages::WebAudioLanguageAction;
    let generation = crate::web_media_stream_model::WebMediaStreamGeneration::for_test(3, 1);
    let original = WebAudioLanguageAction::for_test(generation, 0);
    let russian = WebAudioLanguageAction::for_test(generation, 1);
    let view = AudioTrackMenuView::for_web_languages_test(&[
        (original, "English (original)", true),
        (russian, "Russian", false),
    ]);
    let (note, _) = centers(FULL_WINDOW);
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    scene.left_click(note, &view);
    scene.settle(&view);

    let actions = scene.left_click(scene.center_of("Russian"), &view);

    assert_eq!(
        actions,
        vec![ControlAction::AudioTracks(
            AudioTrackControlAction::SwitchWebAudioLanguage(russian)
        )]
    );
}

/// Сколько языков в «длинном» меню: десятки дубляжей yt-dlp или MKV с 40 дорожками.
const MANY_LANGUAGES: u32 = 40;
/// Запас на рамку попапа и внутренние отступы сверх высоты списка.
const POPUP_CHROME_ALLOWANCE: f32 = 40.0;

/// Меню с `MANY_LANGUAGES` языками; первый — выбранный.
fn many_languages_view() -> AudioTrackMenuView {
    use media_source_open::web_media_open::audio_languages::WebAudioLanguageAction;
    let generation = crate::web_media_stream_model::WebMediaStreamGeneration::for_test(3, 1);
    let labels: Vec<String> = (0..MANY_LANGUAGES)
        .map(|index| format!("Language {index}"))
        .collect();
    let rows: Vec<_> = labels
        .iter()
        .zip(0..)
        .map(|(label, index)| {
            (
                WebAudioLanguageAction::for_test(generation, index),
                label.as_str(),
                index == 0,
            )
        })
        .collect();
    AudioTrackMenuView::for_web_languages_test(&rows)
}

/// Высота самого высокого попапа (слой не ниже `Foreground`) в контексте.
fn tallest_popup_height(context: &egui::Context) -> f32 {
    context.memory(|memory| {
        memory
            .areas()
            .visible_layer_ids()
            .into_iter()
            .filter(|layer| layer.order == egui::Order::Foreground)
            .filter_map(|layer| memory.area_rect(layer.id))
            .map(|rect| rect.height())
            .fold(0.0, f32::max)
    })
}

#[test]
fn menu_with_many_tracks_is_height_bounded_and_scrolls_to_the_last_one() {
    let view = many_languages_view();
    let (note, _) = centers(FULL_WINDOW);
    let mut scene = Scene::new(FULL_WINDOW);
    scene.settle(&view);
    scene.left_click(note, &view);
    scene.settle(&view);
    assert!(scene.popup_open());

    let height = tallest_popup_height(&scene.context);
    assert!(
        height > 0.0
            && height <= super::super::menu::TRACK_LIST_MAX_HEIGHT + POPUP_CHROME_ALLOWANCE,
        "popup height {height} must stay within the list limit"
    );

    // Крутим колесо над списком: последняя строка должна стать видимой в попапе.
    let first_row = scene.center_of("Language 0");
    let wheel = Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -5000.0),
        phase: egui::TouchPhase::Move,
        modifiers: Modifiers::NONE,
    };
    scene.frame(vec![Event::PointerMoved(first_row), wheel], &view);
    scene.settle(&view);
    let last_row = scene.center_of(&format!("Language {}", MANY_LANGUAGES - 1));
    assert!(
        last_row.y < first_row.y + super::super::menu::TRACK_LIST_MAX_HEIGHT,
        "last row {last_row:?} must be scrolled into the visible list"
    );
}
