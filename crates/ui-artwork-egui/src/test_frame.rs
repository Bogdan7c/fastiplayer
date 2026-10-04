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
