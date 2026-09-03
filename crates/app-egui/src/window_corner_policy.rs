//! App-owned policy формы текущего desktop-кадра.

/// Разрешает форму кадра без доступа renderer-а к winit или пользовательскому config-у.
pub(crate) fn resolve_window_corner_mask(
    configured_radius_points: f32,
    is_maximized: bool,
    is_fullscreen: bool,
) -> render_wgpu_shell::WindowCornerMask {
    if is_maximized || is_fullscreen || configured_radius_points <= 0.0 {
        return render_wgpu_shell::WindowCornerMask::square();
    }
    render_wgpu_shell::WindowCornerMask::rounded_in_points(configured_radius_points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_window_uses_configured_corner_radius() {
        assert_eq!(
            resolve_window_corner_mask(12.0, false, false),
            render_wgpu_shell::WindowCornerMask::rounded_in_points(12.0)
        );
        assert_eq!(
            resolve_window_corner_mask(0.0, false, false),
            render_wgpu_shell::WindowCornerMask::square()
        );
    }

    #[test]
    fn exclusive_window_states_are_square_without_losing_configured_radius() {
        assert_eq!(
            resolve_window_corner_mask(24.0, true, false),
            render_wgpu_shell::WindowCornerMask::square()
        );
        assert_eq!(
            resolve_window_corner_mask(24.0, false, true),
            render_wgpu_shell::WindowCornerMask::square()
        );
        assert_eq!(
            resolve_window_corner_mask(24.0, false, false),
            render_wgpu_shell::WindowCornerMask::rounded_in_points(24.0)
        );
    }
}
