//! Колесо мыши и тачпад над областью видео → шаги громкости.

use egui::{Context, Response};

/// Прокрутка в egui points на один шаг громкости: один «щелчок» колеса мыши
/// (дефолт egui для строчной прокрутки — 40 points). Такой же порог у скорости воспроизведения.
const WHEEL_POINTS_PER_VOLUME_STEP: f32 = 40.0;

/// Накопитель плавной прокрутки тачпада между кадрами.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct WheelAccumulator {
    /// Ещё не набравшая целого шага вертикальная прокрутка, points.
    residual_points: f32,
}

impl WheelAccumulator {
    /// Добавляет дельту и возвращает целое число шагов, оставляя остаток.
    pub(super) fn take_steps(&mut self, delta_points: f32) -> i32 {
        self.residual_points += delta_points;
        let steps = (self.residual_points / WHEEL_POINTS_PER_VOLUME_STEP).trunc() as i32;
        self.residual_points -= steps as f32 * WHEEL_POINTS_PER_VOLUME_STEP;
        steps
    }
}

/// Забирает прокрутку колеса, если курсор над областью видео, и возвращает шаги громкости
/// (вверх — положительные). Прокрутка при этом «съедается», чтобы никто ниже по кадру
/// не прокрутил то же самое. Вне области видео накопленный остаток сбрасывается.
pub(super) fn take_volume_steps(egui_ctx: &Context, video_surface: &Response) -> i32 {
    let accumulator_id = video_surface.id.with("volume_wheel_accumulator");
    if !video_surface.hovered() {
        egui_ctx.data_mut(|data| data.remove_temp::<WheelAccumulator>(accumulator_id));
        return 0;
    }
    let delta_points = egui_ctx.input_mut(|input| {
        let delta = input.smooth_scroll_delta().y;
        if delta != 0.0 {
            input.smooth_scroll_delta.y = 0.0;
        }
        delta
    });
    egui_ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<WheelAccumulator>(accumulator_id)
            .take_steps(delta_points)
    })
}
