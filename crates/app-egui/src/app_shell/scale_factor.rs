//! Реакция shell на смену масштаба дисплея (`WindowEvent::ScaleFactorChanged`).
//!
//! Корень прошлой ошибки: `egui-winit` сам кладёт новый масштаб в
//! `viewports[..].native_pixels_per_point` сырого ввода, а egui считает
//! итоговый `pixels_per_point = zoom_factor * native_pixels_per_point`.
//! Ручной `Context::set_pixels_per_point(new)` внутри делает
//! `set_zoom_factor(new / native_старый)`, и на следующем проходе получалось
//! `new * new / old` (100% -> 150% давало 2.25). Масштаб интерфейса
//! (zoom_factor) в приложении не настраивается и должен оставаться 1.0, поэтому
//! единственный владелец масштаба - `egui-winit`, а shell только пишет лог.

use tracing::debug;

/// Обрабатывает смену масштаба дисплея. Масштаб egui НЕ трогает: его уже
/// обновил `egui_winit_state.on_window_event` для этого же события.
pub(super) fn on_scale_factor_changed(_egui_ctx: &egui::Context, scale_factor: f64) {
    // Контекст принимаем намеренно: граница остаётся прежней, а тест гоняет её
    // вместе с проходами egui; менять `pixels_per_point` здесь нельзя (см. выше).
    debug!(scale_factor, "Scale factor changed");
}

#[cfg(test)]
mod tests {
    use super::on_scale_factor_changed;

    /// Один проход egui с заданным родным масштабом окна, как делает egui-winit.
    fn run_pass(egui_ctx: &egui::Context, native_pixels_per_point: f32) {
        let mut raw_input = egui::RawInput::default();
        raw_input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(native_pixels_per_point);
        let mut output = egui_ctx.run_ui(raw_input, |_ui| {});
        // Дельты текстур тестовому проходу не нужны; egui требует явно их сбросить.
        output.textures_delta.clear();
    }

    /// Смена масштаба 1.0 -> 1.5 -> 1.0 даёт ровно родной масштаб окна,
    /// а не квадрат отношения (старый код: 2.25, затем 0.667).
    #[test]
    fn scale_change_gives_native_scale_without_squaring() {
        let egui_ctx = egui::Context::default();
        run_pass(&egui_ctx, 1.0);
        run_pass(&egui_ctx, 1.0);
        assert_eq!(egui_ctx.pixels_per_point(), 1.0);

        // egui-winit обновляет native ppp в вводе, затем shell получает то же событие.
        on_scale_factor_changed(&egui_ctx, 1.5);
        run_pass(&egui_ctx, 1.5);
        run_pass(&egui_ctx, 1.5);
        assert_eq!(egui_ctx.pixels_per_point(), 1.5);
        assert_eq!(egui_ctx.zoom_factor(), 1.0);

        on_scale_factor_changed(&egui_ctx, 1.0);
        run_pass(&egui_ctx, 1.0);
        run_pass(&egui_ctx, 1.0);
        assert_eq!(egui_ctx.pixels_per_point(), 1.0);
        assert_eq!(egui_ctx.zoom_factor(), 1.0);
    }
}
