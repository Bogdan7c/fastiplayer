//! Отрисовка спиннера ожидания данных в центре видео (UX edge cases, сессия 15).
//!
//! Решение «показывать или нет» принимает владелец `state::notifications` (задержка
//! появления, приоритет сообщений центра). Здесь только размещение, угол поворота и
//! политика движения; саму фигуру рисует `ui-artwork-egui` через `ArtworkPainter`.

use std::f64::consts::TAU;

use ui_artwork_egui::{ArtworkPainter, BufferingSpinnerPaintState, BufferingSpinnerStyle};

use crate::ui::animation::UiMotion;

/// Диаметр круглой подложки спиннера: заметен на весь экран, но не закрывает кадр.
const SPINNER_DIAMETER: f32 = 44.0;

/// Скорость вращения дуги: спокойнее стандартного egui-спиннера (1 оборот/с),
/// чтобы долгое ожидание не раздражало.
const SPINNER_REVOLUTIONS_PER_SECOND: f64 = 0.8;

/// Угол неподвижной дуги при reduced motion: разрыв сверху справа, как у «часов».
const REDUCED_MOTION_ROTATION_RADIANS: f32 = -std::f32::consts::FRAC_PI_4;

/// Полупрозрачная тёмная подложка отделяет дугу от светлого кадра и не прячет его.
const SPINNER_BACKDROP_FILL: egui::Color32 = egui::Color32::from_black_alpha(140);

/// Цвет дуги — тот же светло-серый, что у кнопок транспорта.
const SPINNER_ARC_COLOR: egui::Color32 = egui::Color32::from_gray(235);

/// Толщина дуги.
const SPINNER_ARC_WIDTH: f32 = 3.0;

/// Отступ дуги от края подложки.
const SPINNER_ARC_INSET: f32 = 8.0;

/// Рисует спиннер по центру области `ui` (центральной панели над видео).
///
/// При обычном движении дуга вращается и окно просит следующий кадр; при reduced motion
/// дуга неподвижна и перерисовка ради анимации не запрашивается.
pub(crate) fn render_buffering_indicator(ui: &egui::Ui, motion: UiMotion) {
    let spinner_rect =
        egui::Rect::from_center_size(ui.max_rect().center(), egui::Vec2::splat(SPINNER_DIAMETER));
    let frame_time_seconds = ui.input(|input| input.time);
    if motion == UiMotion::Standard {
        ui.ctx().request_repaint();
    }
    ArtworkPainter::new(ui.painter()).buffering_spinner(
        spinner_rect,
        BufferingSpinnerPaintState {
            rotation_radians: spinner_rotation(frame_time_seconds, motion),
        },
        spinner_style(),
    );
}

/// Угол начала дуги для кадра: из времени egui или фиксированный при reduced motion.
fn spinner_rotation(frame_time_seconds: f64, motion: UiMotion) -> f32 {
    match motion {
        UiMotion::Standard => {
            // Остаток считается в f64: время egui растёт часами, и f32 потерял бы плавность.
            let turn_fraction =
                (frame_time_seconds * SPINNER_REVOLUTIONS_PER_SECOND).rem_euclid(1.0);
            (turn_fraction * TAU) as f32
        }
        UiMotion::Reduced => REDUCED_MOTION_ROTATION_RADIANS,
    }
}

/// Визуальные токены спиннера.
fn spinner_style() -> BufferingSpinnerStyle {
    BufferingSpinnerStyle {
        backdrop_fill: SPINNER_BACKDROP_FILL,
        arc_inset: SPINNER_ARC_INSET,
        arc_stroke: egui::Stroke::new(SPINNER_ARC_WIDTH, SPINNER_ARC_COLOR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_rotation_follows_frame_time_and_wraps_each_turn() {
        let quarter_turn_seconds = 0.25 / SPINNER_REVOLUTIONS_PER_SECOND;
        let quarter = spinner_rotation(quarter_turn_seconds, UiMotion::Standard);
        assert!(
            (quarter - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
            "{quarter}"
        );

        let one_turn_seconds = 1.0 / SPINNER_REVOLUTIONS_PER_SECOND;
        let after_long_session = spinner_rotation(
            one_turn_seconds * 10_000.0 + quarter_turn_seconds,
            UiMotion::Standard,
        );
        assert!(
            (after_long_session - quarter).abs() < 1e-4,
            "{after_long_session}"
        );
    }

    #[test]
    fn reduced_rotation_ignores_frame_time() {
        for seconds in [0.0, 0.3, 17.25, 86_400.0] {
            assert_eq!(
                spinner_rotation(seconds, UiMotion::Reduced),
                REDUCED_MOTION_ROTATION_RADIANS
            );
        }
    }
}
