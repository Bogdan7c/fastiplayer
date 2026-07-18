//! Controls скорости воспроизведения для нижней панели.

mod indicator;

pub(super) use indicator::{control_layout, render_reset_button_at, reveal_progress};

use egui::{Key, Modifiers, Ui};

use super::ControlAction;

pub(crate) const PLAYBACK_RATE_STEP_X: f32 = 0.10;
// egui desktop default line-scroll speed: один wheel line-notch соответствует 40 points.
const PLAYBACK_RATE_WHEEL_POINTS_PER_STEP: f32 = 40.0;
/// Накопитель wheel/touchpad-дельты для scoped playback-rate управления.
#[derive(Debug, Default, Clone)]
struct PlaybackRateInputAccumulator {
    /// Неиспользованная вертикальная дельта в egui points.
    residual_vertical_points: f32,
}

impl PlaybackRateInputAccumulator {
    /// Превращает плавную дельту touchpad/wheel в стабильное число дискретных шагов.
    fn consume_vertical_delta(&mut self, vertical_delta_points: f32, points_per_step: f32) -> i32 {
        if vertical_delta_points == 0.0 {
            return 0;
        }

        self.residual_vertical_points += vertical_delta_points;
        let steps = (self.residual_vertical_points / points_per_step).trunc() as i32;

        if steps != 0 {
            self.residual_vertical_points -= steps as f32 * points_per_step;
        }

        steps
    }

    /// Сбрасывает недособранный жест, когда scoped surface больше не активна.
    fn clear(&mut self) {
        self.residual_vertical_points = 0.0;
    }
}

/// Собирает wheel/touchpad и hotkey intent только с play/pause scoped surface.
pub(super) fn collect_input_actions(
    ui: &mut Ui,
    playback_button_response: &egui::Response,
    actions: &mut Vec<ControlAction>,
) {
    let keyboard_scoped_to_playback_button =
        playback_button_response.hovered() || playback_button_response.has_focus();
    let wheel_scoped_to_playback_button = playback_button_response.hovered();
    let accumulator_id = playback_button_response
        .id
        .with("playback_rate_input_accumulator");

    let input_intent = ui.ctx().input_mut(|input| {
        if !keyboard_scoped_to_playback_button && !wheel_scoped_to_playback_button {
            return PlaybackRateInputIntent::default();
        }

        let (decrement_count, increment_count, reset_requested) =
            if keyboard_scoped_to_playback_button {
                (
                    input.count_and_consume_key(Modifiers::NONE, Key::Minus) as i32,
                    (input.count_and_consume_key(Modifiers::NONE, Key::Plus)
                        + input.count_and_consume_key(Modifiers::NONE, Key::Equals))
                        as i32,
                    input.consume_key(Modifiers::NONE, Key::Num0),
                )
            } else {
                (0, 0, false)
            };
        let vertical_scroll_delta = if wheel_scoped_to_playback_button {
            input.smooth_scroll_delta().y
        } else {
            0.0
        };

        if vertical_scroll_delta != 0.0 {
            // Wheel читаем только под курсором, чтобы focused play/pause не крал scroll у других зон.
            input.smooth_scroll_delta.y = 0.0;
        }

        PlaybackRateInputIntent {
            key_step_count: increment_count - decrement_count,
            reset_requested,
            vertical_scroll_delta,
        }
    });

    let wheel_step_count = ui.ctx().data_mut(|data| {
        let accumulator =
            data.get_temp_mut_or_default::<PlaybackRateInputAccumulator>(accumulator_id);

        if !wheel_scoped_to_playback_button {
            accumulator.clear();
            return 0;
        }

        accumulator.consume_vertical_delta(
            input_intent.vertical_scroll_delta,
            PLAYBACK_RATE_WHEEL_POINTS_PER_STEP,
        )
    });

    if input_intent.reset_requested {
        actions.push(ControlAction::ResetPlaybackRate);
        return;
    }

    let total_step_count = input_intent.key_step_count + wheel_step_count;
    if total_step_count != 0 {
        actions.push(ControlAction::AdjustPlaybackRateSteps(total_step_count));
    }
}

/// Сырой intent из egui input, ещё без touchpad accumulation.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct PlaybackRateInputIntent {
    /// Суммарный шаг от клавиш `+`/`-`.
    key_step_count: i32,

    /// Запрошен reset через `0`.
    reset_requested: bool,

    /// Вертикальная wheel/touchpad дельта в egui points.
    vertical_scroll_delta: f32,
}

/// Форматирует скорость так же, как временная кнопка V1.
fn label(rate_multiplier: f32) -> String {
    if (rate_multiplier.fract()).abs() < f32::EPSILON {
        format!("{rate_multiplier:.0}x")
    } else {
        format!("{rate_multiplier:.2}x")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::skin::PlayerSkin;
    use crate::ui::skin::minimal::MinimalSkin;
    use egui::{
        Event, MouseWheelUnit, PointerButton, Pos2, RawInput, Rect, Sense, TouchPhase, pos2, vec2,
    };
    use player_core::{PlaybackRate, PlayerSnapshot};

    fn test_playback_button_rect() -> Rect {
        Rect::from_min_size(pos2(40.0, 40.0), vec2(48.0, 48.0))
    }

    fn test_rate_button_layout(reveal_progress: f32) -> indicator::PlaybackRateControlLayout {
        // Production style фиксирует 48×28 rate geometry и 5-point gap.
        let controls_style = MinimalSkin.controls_style();
        // Базовый Next остаётся на 64 points правее центра Play/Pause.
        let base_next_rect = Rect::from_center_size(
            pos2(
                test_playback_button_rect().center().x
                    + controls_style.transport_button_center_distance,
                test_playback_button_rect().center().y,
            ),
            vec2(
                controls_style.transport_button_size,
                controls_style.transport_button_size,
            ),
        );
        // Далёкий Fullscreen не ограничивает preferred width в focused widget-тестах.
        let fullscreen_rect = Rect::from_center_size(
            pos2(220.0, test_playback_button_rect().center().y),
            vec2(
                controls_style.fullscreen_button_size,
                controls_style.fullscreen_button_size,
            ),
        );
        // Один helper гарантирует ту же layout-формулу, что production render path.
        control_layout(
            test_playback_button_rect(),
            base_next_rect,
            fullscreen_rect,
            controls_style,
            8.0,
            reveal_progress,
        )
    }

    fn test_hover_position() -> Pos2 {
        test_playback_button_rect().center()
    }

    fn test_outside_position() -> Pos2 {
        pos2(4.0, 4.0)
    }

    fn raw_input(events: Vec<Event>) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 200.0))),
            focused: true,
            events,
            ..Default::default()
        }
    }

    fn key_press(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn wheel_start() -> Event {
        Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: vec2(0.0, 0.0),
            phase: TouchPhase::Start,
            modifiers: Modifiers::NONE,
        }
    }

    fn wheel_move(vertical_points: f32) -> Event {
        Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: vec2(0.0, vertical_points),
            phase: TouchPhase::Move,
            modifiers: Modifiers::NONE,
        }
    }

    fn pointer_button(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    fn collect_actions_for_input(
        egui_ctx: &egui::Context,
        input: RawInput,
        request_keyboard_focus: bool,
    ) -> Vec<ControlAction> {
        let mut actions = Vec::new();

        let _ = egui_ctx.run_ui(input, |ui| {
            let playback_button_response =
                ui.allocate_rect(test_playback_button_rect(), Sense::click());

            if request_keyboard_focus {
                playback_button_response.request_focus();
                assert!(
                    playback_button_response.has_focus(),
                    "test setup must prove focused play/pause scope"
                );
            }

            collect_input_actions(ui, &playback_button_response, &mut actions);
        });

        actions
    }

    fn render_rate_button_clicked_for_input(egui_ctx: &egui::Context, input: RawInput) -> bool {
        let mut actions = Vec::new();

        let _ = egui_ctx.run_ui(input, |ui| {
            // Кнопка интерактивна только при реальной non-1x скорости.
            let mut player_snapshot = PlayerSnapshot::empty();
            // Значение соответствует публичной UI-сетке и гарантированно валидно.
            player_snapshot.playback_rate =
                PlaybackRate::new(1.25).expect("valid focused test playback rate");
            // Fully-open layout исключает влияние animation timing из click semantics.
            render_reset_button_at(
                ui,
                test_rate_button_layout(1.0),
                test_playback_button_rect(),
                &player_snapshot,
                MinimalSkin.controls_style(),
                &mut actions,
            );
        });

        actions == vec![ControlAction::ResetPlaybackRate]
    }

    fn render_reveal_progress_for_frame(
        egui_ctx: &egui::Context,
        time_seconds: f64,
        frame_delta_seconds: f32,
        playback_rate: PlaybackRate,
    ) -> f32 {
        // Synthetic frame time делает проверку 250 ms независимой от wall clock теста.
        let mut input = RawInput {
            time: Some(time_seconds),
            predicted_dt: frame_delta_seconds,
            ..RawInput::default()
        };
        // Явный screen rect сохраняет стабильный UI layout между synthetic frames.
        input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, 180.0)));
        // Progress записывается из реального production helper.
        let mut progress = f32::NAN;
        // Один Context сохраняет animation manager state между вызовами.
        let _ = egui_ctx.run_ui(input, |ui| {
            progress = reveal_progress(ui, playback_rate, false);
        });
        // NaN означал бы, что render closure не выполнился.
        assert!(progress.is_finite());
        // Возвращаем eased значение для characterization assertions.
        progress
    }

    /// Проверяет stop gate S38: custom-painted play/pause с `Sense::click()` фокусируем.
    #[test]
    fn egui_click_sense_keeps_custom_playback_button_focusable() {
        assert!(Sense::click().is_focusable());
    }

    /// Проверяет wheel path: один discrete wheel/touchpad шаг даёт один semantic UI intent.
    #[test]
    fn wheel_over_playback_button_emits_one_rate_step() {
        let egui_ctx = egui::Context::default();
        let warmup_actions = collect_actions_for_input(
            &egui_ctx,
            raw_input(vec![Event::PointerMoved(test_hover_position())]),
            false,
        );
        assert!(warmup_actions.is_empty());

        let actions = collect_actions_for_input(
            &egui_ctx,
            raw_input(vec![
                Event::PointerMoved(test_hover_position()),
                wheel_start(),
                wheel_move(PLAYBACK_RATE_WHEEL_POINTS_PER_STEP),
            ]),
            false,
        );

        assert_eq!(actions, vec![ControlAction::AdjustPlaybackRateSteps(1)]);
    }

    /// Wheel вне play/pause не должен становиться playback-rate intent-ом.
    #[test]
    fn wheel_outside_playback_button_does_not_emit_rate_action() {
        let egui_ctx = egui::Context::default();
        let actions = collect_actions_for_input(
            &egui_ctx,
            raw_input(vec![
                Event::PointerMoved(test_outside_position()),
                wheel_start(),
                wheel_move(PLAYBACK_RATE_WHEEL_POINTS_PER_STEP),
            ]),
            false,
        );

        assert!(actions.is_empty());
    }

    /// `+` работает только в scoped surface; `Shift+=` покрывает обычную клавиатуру.
    #[test]
    fn plus_key_requires_hover_or_focus_scope() {
        let egui_ctx = egui::Context::default();
        let unscoped_actions = collect_actions_for_input(
            &egui_ctx,
            raw_input(vec![
                Event::PointerMoved(test_outside_position()),
                key_press(Key::Equals, Modifiers::SHIFT),
            ]),
            false,
        );
        assert!(unscoped_actions.is_empty());

        let scoped_actions = collect_actions_for_input(
            &egui_ctx,
            raw_input(vec![
                Event::PointerMoved(test_hover_position()),
                key_press(Key::Equals, Modifiers::SHIFT),
            ]),
            false,
        );
        assert_eq!(
            scoped_actions,
            vec![ControlAction::AdjustPlaybackRateSteps(1)]
        );
    }

    /// Keyboard focus без hover достаточно для `-`/`0`, но это не расширяет hotkeys глобально.
    #[test]
    fn keyboard_focus_scope_handles_minus_and_reset_without_hover() {
        let egui_ctx = egui::Context::default();
        let minus_actions = collect_actions_for_input(
            &egui_ctx,
            raw_input(vec![
                Event::PointerMoved(test_outside_position()),
                key_press(Key::Minus, Modifiers::NONE),
            ]),
            true,
        );
        assert_eq!(
            minus_actions,
            vec![ControlAction::AdjustPlaybackRateSteps(-1)]
        );

        let reset_actions = collect_actions_for_input(
            &egui_ctx,
            raw_input(vec![
                Event::PointerMoved(test_outside_position()),
                key_press(Key::Num0, Modifiers::NONE),
            ]),
            true,
        );
        assert_eq!(reset_actions, vec![ControlAction::ResetPlaybackRate]);
    }

    /// Проверяет, что egui temp state реально копит smooth wheel между кадрами.
    #[test]
    fn smooth_wheel_accumulates_across_egui_frames() {
        let egui_ctx = egui::Context::default();
        let half_step = PLAYBACK_RATE_WHEEL_POINTS_PER_STEP * 0.5;
        let warmup_actions = collect_actions_for_input(
            &egui_ctx,
            raw_input(vec![Event::PointerMoved(test_hover_position())]),
            false,
        );
        assert!(warmup_actions.is_empty());

        let first_frame_actions = collect_actions_for_input(
            &egui_ctx,
            raw_input(vec![
                Event::PointerMoved(test_hover_position()),
                wheel_start(),
                wheel_move(half_step),
            ]),
            false,
        );
        assert!(first_frame_actions.is_empty());

        let second_frame_actions = collect_actions_for_input(
            &egui_ctx,
            raw_input(vec![
                Event::PointerMoved(test_hover_position()),
                wheel_move(half_step),
            ]),
            false,
        );
        assert_eq!(
            second_frame_actions,
            vec![ControlAction::AdjustPlaybackRateSteps(1)]
        );
    }

    /// Проверяет формат временной V1-кнопки скорости.
    #[test]
    fn playback_rate_label_matches_v1_format() {
        assert_eq!(label(1.0), "1x");
        assert_eq!(label(1.25), "1.25x");
        assert_eq!(label(0.80), "0.80x");
    }

    /// Проверяет, что временная adjacent rate-кнопка является настоящей reset-click surface.
    #[test]
    fn rate_reset_button_reports_click_on_pointer_release() {
        let egui_ctx = egui::Context::default();
        let click_position = test_rate_button_layout(1.0).visible_rect.center();

        let hover_clicked = render_rate_button_clicked_for_input(
            &egui_ctx,
            raw_input(vec![Event::PointerMoved(click_position)]),
        );
        assert!(!hover_clicked);

        let press_clicked = render_rate_button_clicked_for_input(
            &egui_ctx,
            raw_input(vec![pointer_button(click_position, true)]),
        );
        assert!(!press_clicked);

        let release_clicked = render_rate_button_clicked_for_input(
            &egui_ctx,
            raw_input(vec![pointer_button(click_position, false)]),
        );
        assert!(release_clicked);
    }

    #[test]
    fn normal_rate_closing_outline_has_no_label_or_reset_interaction() {
        // Fully-visible layout моделирует первый кадр closing после snapshot=1x.
        let layout = test_rate_button_layout(1.0);
        // Empty snapshot по контракту имеет NORMAL playback rate.
        let player_snapshot = PlayerSnapshot::empty();
        // Typed actions должны остаться пустыми даже при видимом closing-контуре.
        let mut actions = Vec::new();
        // Paint output позволяет отличить outline от отсутствующей текстовой shape.
        let output = egui::Context::default().run_ui(RawInput::default(), |ui| {
            render_reset_button_at(
                ui,
                layout,
                test_playback_button_rect(),
                &player_snapshot,
                MinimalSkin.controls_style(),
                &mut actions,
            );
        });

        // Единственный shape — пустой outline; надпись `1x` не рисуется.
        assert_eq!(output.shapes.len(), 1);
        // Closing control не может породить повторный reset.
        assert!(actions.is_empty());
    }

    #[test]
    fn playback_rate_reveal_uses_full_250_millisecond_transition() {
        // Общий Context хранит один stable animation Id между synthetic frames.
        let egui_ctx = egui::Context::default();
        // Первый NORMAL кадр регистрирует скрытое начальное состояние.
        let hidden = render_reveal_progress_for_frame(&egui_ctx, 0.0, 0.125, PlaybackRate::NORMAL);
        // Первый non-1x кадр через 125 ms должен оказаться ровно посередине cubic curve.
        let halfway = render_reveal_progress_for_frame(
            &egui_ctx,
            0.125,
            0.125,
            PlaybackRate::new(1.25).expect("valid test rate"),
        );
        // Вторые 125 ms завершают полный согласованный 250-ms переход.
        let open = render_reveal_progress_for_frame(
            &egui_ctx,
            0.250,
            0.125,
            PlaybackRate::new(1.25).expect("valid test rate"),
        );

        // NORMAL нигде не раскрывает control.
        assert_eq!(hidden, 0.0);
        // Cubic ease-in-out симметричен и имеет midpoint 0.5.
        assert!((halfway - 0.5).abs() < 0.0001);
        // Полная длительность заканчивается точным open состоянием.
        assert_eq!(open, 1.0);
    }

    #[test]
    fn reduced_motion_makes_playback_rate_reveal_instant() {
        let context = egui::Context::default();
        let mut hidden = f32::NAN;
        let mut visible = f32::NAN;

        let _ = context.run_ui(RawInput::default(), |ui| {
            hidden = reveal_progress(ui, PlaybackRate::NORMAL, true);
        });
        let _ = context.run_ui(RawInput::default(), |ui| {
            visible = reveal_progress(ui, PlaybackRate::new(1.25).expect("valid test rate"), true);
        });

        assert_eq!(hidden, 0.0);
        assert_eq!(visible, 1.0);
    }

    #[test]
    fn playback_rate_reveal_reverses_without_visual_jump() {
        // Один Context сохраняет внутреннюю линейную позицию при смене target.
        let egui_ctx = egui::Context::default();
        // 62.5-ms кадры делят полный переход на четыре одинаковых шага.
        let frame_delta = 0.0625;
        // Сначала регистрируем скрытое состояние.
        let _ = render_reveal_progress_for_frame(&egui_ctx, 0.0, frame_delta, PlaybackRate::NORMAL);
        // Четыре шага полностью раскрывают кнопку.
        for frame_index in 1..=4 {
            let _ = render_reveal_progress_for_frame(
                &egui_ctx,
                frame_index as f64 * f64::from(frame_delta),
                frame_delta,
                PlaybackRate::new(1.25).expect("valid test rate"),
            );
        }
        // Один closing-шаг возвращает внутреннюю позицию с 1.0 на 0.75.
        let closing_progress =
            render_reveal_progress_for_frame(&egui_ctx, 0.3125, frame_delta, PlaybackRate::NORMAL);
        // Реверс в тот же момент не продвигает время и обязан сохранить exact visual progress.
        let reversed_progress = render_reveal_progress_for_frame(
            &egui_ctx,
            0.3125,
            frame_delta,
            PlaybackRate::new(1.25).expect("valid test rate"),
        );

        // Target-aware cubic easing симметричен, поэтому при реверсе нет скачка.
        assert!((closing_progress - reversed_progress).abs() < 0.0001);
    }

    /// Проверяет, что плавный touchpad не даёт jitter до пересечения threshold.
    #[test]
    fn playback_rate_accumulator_waits_until_threshold() {
        let mut accumulator = PlaybackRateInputAccumulator::default();
        let points_per_step = PLAYBACK_RATE_WHEEL_POINTS_PER_STEP;

        assert_eq!(
            accumulator.consume_vertical_delta(points_per_step * 0.5, points_per_step),
            0
        );
        assert_eq!(
            accumulator.consume_vertical_delta(points_per_step * 0.5 - 1.0, points_per_step),
            0
        );
        assert_eq!(accumulator.consume_vertical_delta(1.0, points_per_step), 1);
        assert_eq!(
            accumulator.consume_vertical_delta(points_per_step - 1.0, points_per_step),
            0
        );
        assert_eq!(accumulator.consume_vertical_delta(1.0, points_per_step), 1);
    }

    /// Проверяет точное число шагов и сохранение остатка у smooth wheel/touchpad.
    #[test]
    fn playback_rate_accumulator_emits_exact_step_count_and_keeps_residual() {
        let mut accumulator = PlaybackRateInputAccumulator::default();
        let points_per_step = PLAYBACK_RATE_WHEEL_POINTS_PER_STEP;

        assert_eq!(
            accumulator.consume_vertical_delta(points_per_step * 2.5, points_per_step),
            2
        );
        assert_eq!(
            accumulator.consume_vertical_delta(points_per_step * 0.49, points_per_step),
            0
        );
        assert_eq!(
            accumulator.consume_vertical_delta(points_per_step * 0.01, points_per_step),
            1
        );
        assert_eq!(
            accumulator.consume_vertical_delta(-points_per_step * 3.0, points_per_step),
            -3
        );
    }

    /// Проверяет fully-open геометрию и одинаковое смещение Next.
    #[test]
    fn playback_rate_button_sits_between_playback_and_fixed_repeat_button() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let playback_button_rect = Rect::from_center_size(
            pos2(
                row_rect.center().x,
                row_rect.center().y - controls_style.playback_button_vertical_raise,
            ),
            vec2(
                controls_style.playback_button_diameter,
                controls_style.playback_button_diameter,
            ),
        );
        let repeat_button_rect = Rect::from_center_size(
            pos2(
                playback_button_rect.center().x + controls_style.queue_mode_button_center_distance,
                playback_button_rect.center().y,
            ),
            vec2(
                controls_style.transport_button_size,
                controls_style.transport_button_size,
            ),
        );
        let base_next_button_rect =
            super::super::transport::next_button_rect(playback_button_rect, controls_style);
        let layout = control_layout(
            playback_button_rect,
            base_next_button_rect,
            repeat_button_rect,
            controls_style,
            8.0,
            1.0,
        );

        assert_eq!(
            layout.button_rect.left(),
            playback_button_rect.right() + controls_style.playback_rate_button_gap
        );
        assert_eq!(
            layout.button_rect.width(),
            controls_style.playback_rate_button_width
        );
        assert_eq!(
            layout.button_rect.height(),
            controls_style.transport_button_size
                - controls_style.playback_rate_button_vertical_inset * 2.0
        );
        assert_eq!(layout.visible_rect, layout.button_rect);
        assert_eq!(
            layout.next_button_rect.left() - base_next_button_rect.left(),
            layout.button_rect.width()
        );
        assert!(layout.button_rect.right() <= layout.next_button_rect.left());
        assert!(
            layout.next_button_rect.right() + controls_style.queue_mode_neighbor_gap
                <= repeat_button_rect.left()
        );
    }

    #[test]
    fn playback_rate_layout_moves_indicator_and_next_with_one_progress() {
        // Три контрольных точки характеризуют hidden, half-open и fully-open layout.
        let hidden = test_rate_button_layout(0.0);
        let halfway = test_rate_button_layout(0.5);
        let open = test_rate_button_layout(1.0);
        // Preferred width известна из production MinimalSkin.
        let full_width = MinimalSkin.controls_style().playback_rate_button_width;

        // Hidden control не оставляет hit-area.
        assert_eq!(hidden.visible_rect.width(), 0.0);
        // Half-open control раскрывает ровно половину preferred width.
        assert!((halfway.visible_rect.width() - full_width * 0.5).abs() < 0.0001);
        // Fully-open control раскрывает всю ширину.
        assert!((open.visible_rect.width() - full_width).abs() < 0.0001);
        // Next на половине пути получает то же смещение, что и правая грань indicator-а.
        assert!(
            (halfway.next_button_rect.left()
                - hidden.next_button_rect.left()
                - halfway.visible_rect.width())
            .abs()
                < 0.0001
        );
        // Полный сдвиг Next также совпадает с полной раскрытой шириной.
        assert!(
            (open.next_button_rect.left()
                - hidden.next_button_rect.left()
                - open.visible_rect.width())
            .abs()
                < 0.0001
        );
    }
}
