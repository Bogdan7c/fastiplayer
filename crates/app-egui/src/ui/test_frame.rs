//! Тестовый прогон одного кадра egui.
//!
//! egui 0.36 в debug-сборке паникует, если `TexturesDelta` уничтожается с
//! неприменёнными обновлениями текстур. Headless-тесты не загружают текстуры
//! на GPU, поэтому кадр прогоняется через этот хелпер: он явно отказывается
//! от обновлений текстур, а shapes/platform output оставляет тесту.

/// Прогоняет кадр `egui_ctx.run_ui` и возвращает вывод без обновлений текстур.
pub(crate) fn run_ui_frame(
    egui_ctx: &egui::Context,
    input: egui::RawInput,
    add_contents: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut full_output = egui_ctx.run_ui(input, add_contents);
    // Тест не владеет GPU-текстурами: обновления атласа не применяются, как и до egui 0.36.
    full_output.textures_delta.clear();
    full_output
}

/// Контекст egui с поведением приложения, как его создаёт `AppState::new`.
///
/// Нужен тестам, которые проверяют запросы перерисовки: голый
/// `egui::Context::default()` в egui 0.36 сам просит перерисовку из-за
/// синхронизации темы окна, которую приложение выключает.
pub(crate) fn app_behavior_context() -> egui::Context {
    let egui_ctx = egui::Context::default();
    crate::ui::egui_behavior::apply_app_egui_behavior(&egui_ctx);
    egui_ctx
}

/// Сырой ввод одного тестового кадра: окно 800×600 точек, момент `time_seconds`.
pub(crate) fn input_at(time_seconds: f64, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        time: Some(time_seconds),
        events,
        ..Default::default()
    }
}

/// Последовательность кадров «навести и кликнуть `click_count` раз» в точке `position`.
///
/// Клики идут с интервалом 50 мс — быстрее порога двойного клика egui.
pub(crate) fn click_frames(position: egui::Pos2, click_count: usize) -> Vec<egui::RawInput> {
    let button_event = |pressed| egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let mut frames = vec![input_at(0.0, vec![egui::Event::PointerMoved(position)])];
    for click_index in 0..click_count {
        let click_start = 0.05 + click_index as f64 * 0.1;
        frames.push(input_at(click_start, vec![button_event(true)]));
        frames.push(input_at(click_start + 0.05, vec![button_event(false)]));
    }
    frames
}
