use super::*;

/// Отрицательный и нечисловой радиус безопасно превращаются в прямые углы.
#[test]
fn invalid_radius_disables_rounding() {
    assert_eq!(
        WindowCornerMask::rounded_in_points(-4.0).radius_points(),
        0.0
    );
    assert_eq!(
        WindowCornerMask::rounded_in_points(f32::NAN).radius_points(),
        0.0
    );
    assert_eq!(
        WindowCornerMask::rounded_in_points(f32::INFINITY).radius_points(),
        0.0
    );
}

/// Обычный радиус сохраняется в точках egui; прямой контур — нулевой радиус.
#[test]
fn rounded_radius_is_kept_in_points() {
    assert_eq!(
        WindowCornerMask::rounded_in_points(10.5).radius_points(),
        10.5
    );
    assert_eq!(
        WindowCornerMask::square(),
        WindowCornerMask::rounded_in_points(0.0)
    );
}
