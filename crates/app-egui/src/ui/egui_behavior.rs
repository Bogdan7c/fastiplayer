//! Поведение egui, которое приложение считает своим контрактом.
//!
//! Дефолты egui меняются между версиями (0.35 замедлил стандартные анимации,
//! убрал запас clip rect у `ScrollArea`, отключил прокрутку перетаскиванием
//! мышью и сменил отрисовку IME-композиции). Решение владельца при обновлении
//! egui 0.34 → 0.35: пользователь должен видеть и делать то же, что раньше.
//! Поэтому прежние значения зафиксированы здесь явно, в одном месте, а не
//! размазаны по виджетам.
//!
//! egui 0.36 добавил ещё одно изменение: синхронизацию темы окна
//! (`sync_window_theme`). Она тоже выключена здесь — см.
//! `apply_app_egui_behavior`.
//!
//! Исключение: запас обрезки 3 pt у краёв `ScrollArea` (`clip_rect_margin`).
//! egui 0.36 сделал эту настройку неработающей, и вернуть её нечем: обводка
//! или подсветка у самого края прокручиваемого списка может срезаться на
//! ≤3 pt. Владелец принял это отличие (4 октября 2026).
//!
//! Модуль владеет только глобальными настройками поведения egui. Цвета,
//! отступы и размеры controls остаются в `ui::skin`.

use egui::scroll_area::ScrollSource;

/// Длительность стандартных анимаций egui в секундах.
///
/// Влияет на всё, что анимирует сам egui по `Style::animation_time`:
/// плавное появление popup-меню, tooltip-ов, ComboBox и `animate_bool`.
/// egui 0.34 использовал 6/60 с (0.1 с), egui 0.35 по умолчанию — 0.2 с.
/// Собственные анимации приложения (`ui::animation`) задают время явно и от
/// этого значения не зависят.
const APP_ANIMATION_TIME_SECONDS: f32 = 6.0 / 60.0;

/// Источник прокрутки для всех `ScrollArea` приложения.
///
/// `ALL` = полоса прокрутки + колесо мыши + перетаскивание содержимого
/// **в том числе мышью**. Так было по умолчанию в egui 0.34; в egui 0.35
/// перетаскивание по умолчанию работает только на touch-экранах.
/// Виджеты, которые сами захватывают drag (например, перестановка строк
/// плейлиста), по-прежнему имеют приоритет над прокруткой.
const APP_SCROLL_SOURCE: ScrollSource = ScrollSource::ALL;

/// Применяет к контексту egui прежнее (egui 0.34) поведение приложения.
///
/// Вызывается один раз при создании `egui::Context` в `AppState::new`.
/// Меняет стили обеих тем (тёмной и светлой), чтобы смена темы не вернула
/// дефолты egui.
pub(crate) fn apply_app_egui_behavior(egui_ctx: &egui::Context) {
    egui_ctx.all_styles_mut(|style| {
        style.animation_time = APP_ANIMATION_TIME_SECONDS;
        // egui 0.35 на Linux рисует IME-композицию по-новому (подчёркивание,
        // курсор внутри композиции). `legacy_visuals` возвращает прежний вид.
        style.visuals.ime_composition.legacy_visuals = true;
    });
    // egui 0.36 по умолчанию сам отправляет окну `ViewportCommand::SetTheme`,
    // чтобы системные декорации следовали теме egui, и ради этого запрашивает
    // лишнюю перерисовку. egui 0.35 тему окна не трогал; оставляем как было.
    egui_ctx.options_mut(|options| options.sync_window_theme = false);
}

/// Вертикальная `ScrollArea` с прокруткой, как в остальном приложении.
///
/// Все вертикальные области прокрутки приложения создаются только через эту
/// функцию (это проверяет тест), чтобы источник прокрутки не разъехался между
/// панелями при следующем обновлении egui.
pub(crate) fn vertical_scroll_area() -> egui::ScrollArea {
    egui::ScrollArea::vertical().scroll_source(APP_SCROLL_SOURCE)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use egui::{Event, PointerButton, RawInput, Rect, pos2, vec2};

    use super::*;

    /// Длительность одного кадра в тестах: стандартные 60 Гц.
    const FRAME_SECONDS: f64 = 1.0 / 60.0;

    /// Размер тестового экрана в points.
    const SCREEN_SIZE: egui::Vec2 = vec2(400.0, 300.0);

    /// Высота содержимого заведомо больше экрана, чтобы было что прокручивать.
    const TALL_CONTENT_HEIGHT: f32 = 2_000.0;

    /// Насколько тянем содержимое мышью вверх.
    const DRAG_DISTANCE_POINTS: f32 = 80.0;

    fn app_context() -> egui::Context {
        let egui_ctx = egui::Context::default();
        apply_app_egui_behavior(&egui_ctx);
        egui_ctx
    }

    fn frame_input(events: Vec<Event>, time_seconds: f64) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, SCREEN_SIZE)),
            time: Some(time_seconds),
            focused: true,
            events,
            ..RawInput::default()
        }
    }

    /// Стандартная анимация egui должна доходить до конца за 0.1 с (6 кадров
    /// при 60 Гц), как в egui 0.34. С дефолтом egui 0.35 за то же время
    /// было бы пройдено только 50 %.
    #[test]
    fn standard_egui_animation_completes_in_legacy_time() {
        let egui_ctx = app_context();
        let animation_id = egui::Id::new("legacy_animation_time_probe");
        let legacy_frame_count = 6;

        // Кадр 0: анимация в состоянии "выключено".
        let _ =
            crate::ui::test_frame::run_ui_frame(&egui_ctx, frame_input(Vec::new(), 0.0), |ui| {
                let _ = ui.ctx().animate_bool(animation_id, false);
            });

        // Кадры 1..=6: включаем и смотрим, где анимация окажется через 0.1 с.
        let mut animated_value = 0.0;
        for frame_index in 1..=legacy_frame_count {
            let time_seconds = f64::from(frame_index) * FRAME_SECONDS;
            let _ = crate::ui::test_frame::run_ui_frame(
                &egui_ctx,
                frame_input(Vec::new(), time_seconds),
                |ui| {
                    animated_value = ui.ctx().animate_bool(animation_id, true);
                },
            );
        }

        assert!(
            animated_value >= 0.99,
            "анимация должна завершиться за 0.1 с, получено {animated_value}"
        );
    }

    /// egui не отправляет окну команду смены темы и не просит из-за неё
    /// перерисовку, как в egui 0.35 (в 0.36 это делает `sync_window_theme`).
    #[test]
    fn egui_does_not_push_theme_to_native_window() {
        let egui_ctx = app_context();
        // Приложение выбирает тёмную тему так же, как `AppState::new`.
        egui_ctx.set_theme(egui::Theme::Dark);

        let full_output =
            crate::ui::test_frame::run_ui_frame(&egui_ctx, frame_input(Vec::new(), 0.0), |ui| {
                ui.label("theme probe");
            });

        let sent_theme_commands = full_output
            .viewport_output
            .values()
            .flat_map(|viewport_output| &viewport_output.commands)
            .filter(|command| matches!(command, egui::ViewportCommand::SetTheme(_)))
            .count();
        assert_eq!(
            sent_theme_commands, 0,
            "egui не должен менять тему окна сам"
        );
    }

    /// Перетаскивание содержимого обычной мышью (не touch) прокручивает
    /// область, как в egui 0.34.
    #[test]
    fn mouse_drag_scrolls_scroll_area_content() {
        let egui_ctx = app_context();
        let drag_start = pos2(100.0, 200.0);
        let drag_end = pos2(100.0, 200.0 - DRAG_DISTANCE_POINTS);
        // egui ищет цель нажатия среди виджетов прошлого кадра, а drag-зону
        // ScrollArea регистрирует только когда уже знает, что содержимое не
        // помещается. Поэтому два кадра прогрева до нажатия.
        let frames = [
            Vec::new(),
            vec![Event::PointerMoved(drag_start)],
            vec![pointer_button(drag_start, true)],
            vec![Event::PointerMoved(pos2(100.0, 160.0))],
            vec![Event::PointerMoved(drag_end)],
            vec![pointer_button(drag_end, false)],
        ];

        let mut scroll_offset_y = 0.0;
        for (frame_index, events) in frames.into_iter().enumerate() {
            let time_seconds = frame_index as f64 * FRAME_SECONDS;
            let _ = crate::ui::test_frame::run_ui_frame(
                &egui_ctx,
                frame_input(events, time_seconds),
                |ui| {
                    // Область на всю ширину экрана (как у плейлиста), чтобы полоса
                    // прокрутки была у правого края, а не под курсором: иначе тест
                    // измерил бы клик по полосе, а не перетаскивание содержимого.
                    let scroll_output =
                        vertical_scroll_area()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                // Пустое пространство без собственного drag-sense:
                                // перетаскивание может забрать только сама ScrollArea.
                                ui.allocate_space(vec2(100.0, TALL_CONTENT_HEIGHT));
                            });
                    scroll_offset_y = scroll_output.state.offset.y;
                },
            );
        }

        assert!(
            scroll_offset_y >= DRAG_DISTANCE_POINTS * 0.5,
            "перетаскивание мышью вверх должно прокрутить содержимое, offset = {scroll_offset_y}"
        );
    }

    /// IME-композиция (набор через fcitx/ibus) выглядит как в egui 0.34:
    /// как выделение текста, без новых подчёркиваний egui 0.35.
    #[test]
    fn ime_composition_keeps_legacy_selection_look() {
        let egui_ctx = app_context();
        let text_edit_id = egui::Id::new("legacy_ime_probe");
        let mut text = String::new();
        let frames = [
            // Кадр 0: виджет появляется и получает фокус.
            Vec::new(),
            // Кадр 1: метод ввода присылает незавершённый (preedit) текст.
            vec![Event::Ime(egui::ImeEvent::Preedit {
                text: "при".to_owned(),
                active_range_chars: None,
            })],
            // Кадр 2: отрисовка с уже вставленной композицией.
            Vec::new(),
        ];

        let mut last_output = None;
        for (frame_index, events) in frames.into_iter().enumerate() {
            let time_seconds = frame_index as f64 * FRAME_SECONDS;
            let output = crate::ui::test_frame::run_ui_frame(
                &egui_ctx,
                frame_input(events, time_seconds),
                |ui| {
                    let response = ui.add(egui::TextEdit::singleline(&mut text).id(text_edit_id));
                    if frame_index == 0 {
                        response.request_focus();
                    }
                },
            );
            last_output = Some(output);
        }

        // Композиция действительно дошла до поля: иначе проверка ниже пустая.
        assert_eq!(text, "при");
        let underline_color = egui_ctx
            .global_style()
            .visuals
            .ime_composition
            .inactive_underline_stroke
            .color;
        let painted_underline = last_output
            .map(|output| painted_line_colors(&output).contains(&underline_color))
            .unwrap_or(false);
        assert!(
            !painted_underline,
            "IME-композиция не должна рисоваться новым подчёркиванием egui 0.35"
        );
    }

    /// Цвета всех отрезков линий, нарисованных за кадр (подчёркивания IME
    /// egui рисует именно отрезками `line_segment`).
    fn painted_line_colors(output: &egui::FullOutput) -> Vec<egui::Color32> {
        let mut colors = Vec::new();
        for clipped_shape in &output.shapes {
            collect_line_colors(&clipped_shape.shape, &mut colors);
        }
        colors
    }

    fn collect_line_colors(shape: &egui::Shape, colors: &mut Vec<egui::Color32>) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for nested_shape in shapes {
                    collect_line_colors(nested_shape, colors);
                }
            }
            egui::Shape::LineSegment { stroke, .. } => colors.push(stroke.color),
            _ => {}
        }
    }

    fn pointer_button(position: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Ни одна `ScrollArea` приложения не создаётся в обход
    /// `vertical_scroll_area()`: иначе она молча получит дефолты egui.
    #[test]
    fn app_scroll_areas_are_created_only_through_shared_constructor() {
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let this_module = source_root.join("ui").join("egui_behavior.rs");
        // Строки собраны из частей, чтобы тест не находил сам себя.
        let direct_constructor = format!("{}{}", "ScrollArea::", "vertical(");
        let shared_constructor = format!("{}{}", "vertical_scroll_", "area()");

        let mut offenders = Vec::new();
        let mut shared_constructor_uses = 0;
        for path in rust_sources(&source_root) {
            if path == this_module {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("app-egui source is readable");
            if source.contains(&direct_constructor) {
                offenders.push(path.clone());
            }
            shared_constructor_uses += source.matches(&shared_constructor).count();
        }

        assert!(
            offenders.is_empty(),
            "используйте ui::egui_behavior::vertical_scroll_area(): {offenders:?}"
        );
        // Сканер действительно видит панели приложения (настройки x2, URL,
        // сайдбар, плейлист, телеметрия), а не прошёл по пустому каталогу.
        assert_eq!(shared_constructor_uses, 6);
    }

    fn rust_sources(directory: &Path) -> Vec<PathBuf> {
        let mut sources = Vec::new();
        let entries = std::fs::read_dir(directory).expect("source directory is readable");
        for entry in entries {
            let path = entry.expect("source directory entry is readable").path();
            if path.is_dir() {
                sources.extend(rust_sources(&path));
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                sources.push(path);
            }
        }
        sources
    }
}
