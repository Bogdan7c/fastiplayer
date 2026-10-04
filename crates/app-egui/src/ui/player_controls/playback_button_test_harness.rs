//! Тестовый стенд настоящей кнопки play/pause для проверок клавиатуры.
//!
//! Рендерит production-функцию `render_playback_toggle_button_at` и считает
//! её клики: в `render_button_row` каждый клик — это ровно одно
//! `TransportControlAction::TogglePlayback`. Нужен тестам `app_shell::hotkeys`,
//! которые проверяют связку «кнопка → фокус egui → хоткеи» целиком.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2};

use super::render_playback_toggle_button_at;
use crate::ui::assets::IconId;
use crate::ui::skin::minimal::MinimalSkin;
use crate::ui::test_frame::{app_behavior_context, run_ui_frame};

/// Одна кнопка play/pause в собственном контексте egui.
pub(crate) struct PlaybackButtonHarness {
    egui_ctx: egui::Context,
}

impl PlaybackButtonHarness {
    pub(crate) fn new() -> Self {
        let harness = Self {
            egui_ctx: app_behavior_context(),
        };
        // Первый кадр регистрирует виджет, как первый кадр живого окна.
        let _ = harness.run_frame(Vec::new());
        harness
    }

    pub(crate) fn egui_ctx(&self) -> &egui::Context {
        &self.egui_ctx
    }

    /// Клик мышью: наведение, нажатие и отпускание отдельными кадрами.
    ///
    /// Возвращает число переключений playback, которые дала кнопка.
    pub(crate) fn click_with_mouse(&self) -> usize {
        let center = Self::button_rect().center();
        let mut toggles = self.run_frame(vec![Event::PointerMoved(center)]);
        toggles += self.run_frame(vec![Self::primary_button(center, true)]);
        toggles += self.run_frame(vec![Self::primary_button(center, false)]);
        toggles
    }

    /// Выбор кнопки клавиатурой (Tab), как это делает пользователь без мыши.
    pub(crate) fn focus_with_tab(&self) {
        let _ = self.run_frame(vec![Self::key_press(Key::Tab)]);
    }

    /// Нажатие клавиши в кадре egui; возвращает число переключений от кнопки.
    pub(crate) fn press_key(&self, key: Key) -> usize {
        self.run_frame(vec![Self::key_press(key)])
    }

    fn run_frame(&self, events: Vec<Event>) -> usize {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(200.0, 200.0))),
            events,
            ..RawInput::default()
        };
        let mut playback_toggles = 0;
        let _ = run_ui_frame(&self.egui_ctx, input, |ui| {
            let response = render_playback_toggle_button_at(
                ui,
                Self::button_rect(),
                IconId::Play,
                &MinimalSkin,
            );
            // То же правило, что в `render_button_row`: клик = TogglePlayback.
            if response.clicked() {
                playback_toggles += 1;
            }
        });
        playback_toggles
    }

    fn button_rect() -> Rect {
        Rect::from_center_size(pos2(100.0, 100.0), vec2(40.0, 40.0))
    }

    fn primary_button(position: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    fn key_press(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }
}
