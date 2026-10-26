//! Форма desktop-кадра (скругление углов) и подсказка композитору о непрозрачности.
//!
//! Форма решается по committed config и текущему состоянию native окна; та же форма
//! синхронизирует подсказку `crate::window_surface_opacity`, чтобы композитор не
//! перерисовывал окна под квадратным (непрозрачным) кадром.

use super::AppState;

impl AppState {
    /// Разрешает committed контур с учётом текущего native window state.
    pub(crate) fn window_corner_mask(
        &self,
        window: &winit::window::Window,
    ) -> render_shell_api::WindowCornerMask {
        crate::window_corner_policy::resolve_window_corner_mask(
            self.committed_config_snapshot.window_corner_radius_points(),
            window.is_maximized(),
            window.fullscreen().is_some(),
        )
    }

    /// Форма кадра для рендера; композитор получает подсказку о непрозрачности той же формы.
    ///
    /// Квадратный кадр (фуллскрин, развёрнутое окно) объявляется непрозрачным, чтобы
    /// композитор не перерисовывал окна под нами (см. `crate::window_surface_opacity`).
    /// Смена размера окна сама запускает полный проход UI (`WindowFacts::inner_size`
    /// в модели кадра), поэтому на X11 регион догоняет новый размер на ближайшем кадре.
    pub(crate) fn frame_corner_mask(
        &mut self,
        window: &winit::window::Window,
    ) -> render_shell_api::WindowCornerMask {
        let mask = self.window_corner_mask(window);
        self.surface_opacity.sync(mask, window);
        mask
    }
}
