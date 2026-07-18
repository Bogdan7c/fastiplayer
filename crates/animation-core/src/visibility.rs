//! Чистая математика visibility-эффектов без UI toolkit и системных часов.

/// Визуальный эффект, которым UI показывает или скрывает элемент.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VisibilityEffect {
    /// Меняется только прозрачность; геометрический масштаб равен единице.
    Fade,
    /// Вместе с прозрачностью содержимое растёт от `hidden_scale` до единицы.
    FadeScale {
        /// Масштаб полностью скрытого содержимого.
        ///
        /// Значение нормализуется в `0.0..=1.0`, поэтому эффект никогда
        /// не отражает содержимое и не создаёт overshoot выше полного размера.
        hidden_scale: f32,
    },
}

/// Готовый toolkit-neutral результат visibility-анимации.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisibilitySample {
    /// Итоговая прозрачность в закрытом диапазоне `0.0..=1.0`.
    pub opacity: f32,
    /// Итоговый масштаб содержимого в закрытом диапазоне `0.0..=1.0`.
    pub scale: f32,
}

impl VisibilityEffect {
    /// Преобразует нормализованный progress в безопасный paint sample.
    ///
    /// `progress = 0.0` означает полностью скрытое состояние, а `1.0` —
    /// полностью видимое. NaN считается скрытым состоянием; бесконечности
    /// и конечные значения за диапазоном зажимаются на ближайшей границе.
    #[must_use]
    pub fn sample(self, progress: f32) -> VisibilitySample {
        // NaN нельзя передавать в цвета или геометрию, поэтому он безопасно
        // соответствует началу перехода.
        let normalized_progress = if progress.is_nan() {
            0.0
        } else {
            progress.clamp(0.0, 1.0)
        };

        // Fade не меняет размер содержимого даже в полностью скрытом состоянии.
        let scale = match self {
            Self::Fade => 1.0,
            Self::FadeScale { hidden_scale } => {
                // Некорректный scale деградирует до единицы: fade остаётся
                // рабочим, а paint boundary не получает NaN.
                let normalized_hidden_scale = if hidden_scale.is_nan() {
                    1.0
                } else {
                    hidden_scale.clamp(0.0, 1.0)
                };
                // Линейная интерполяция выполняется уже после внешнего easing,
                // поэтому effect не навязывает вызывающей стороне кривую времени.
                normalized_hidden_scale + (1.0 - normalized_hidden_scale) * normalized_progress
            }
        };

        VisibilitySample {
            opacity: normalized_progress,
            scale,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{VisibilityEffect, VisibilitySample};

    /// Допуск нужен только для арифметики `f32`, а не для размытой семантики.
    const EPSILON: f32 = 1e-6;

    /// Проверяет оба поля sample без скрытого округления в assertions.
    fn assert_sample(actual: VisibilitySample, expected_opacity: f32, expected_scale: f32) {
        assert!((actual.opacity - expected_opacity).abs() < EPSILON);
        assert!((actual.scale - expected_scale).abs() < EPSILON);
    }

    #[test]
    fn fade_changes_only_opacity_at_boundaries_and_midpoint() {
        let effect = VisibilityEffect::Fade;

        assert_sample(effect.sample(0.0), 0.0, 1.0);
        assert_sample(effect.sample(0.5), 0.5, 1.0);
        assert_sample(effect.sample(1.0), 1.0, 1.0);
    }

    #[test]
    fn fade_scale_interpolates_from_eighty_percent_to_full_size() {
        let effect = VisibilityEffect::FadeScale { hidden_scale: 0.80 };

        assert_sample(effect.sample(0.0), 0.0, 0.80);
        assert_sample(effect.sample(0.5), 0.5, 0.90);
        assert_sample(effect.sample(1.0), 1.0, 1.00);
    }

    #[test]
    fn progress_is_clamped_and_nan_is_safe() {
        let effect = VisibilityEffect::FadeScale { hidden_scale: 0.80 };

        assert_sample(effect.sample(-5.0), 0.0, 0.80);
        assert_sample(effect.sample(f32::NEG_INFINITY), 0.0, 0.80);
        assert_sample(effect.sample(f32::NAN), 0.0, 0.80);
        assert_sample(effect.sample(5.0), 1.0, 1.00);
        assert_sample(effect.sample(f32::INFINITY), 1.0, 1.00);
    }

    #[test]
    fn hidden_scale_is_sanitized_without_overshoot() {
        let too_large = VisibilityEffect::FadeScale { hidden_scale: 1.50 };
        let negative = VisibilityEffect::FadeScale {
            hidden_scale: -0.25,
        };
        let not_a_number = VisibilityEffect::FadeScale {
            hidden_scale: f32::NAN,
        };

        assert_sample(too_large.sample(0.5), 0.5, 1.0);
        assert_sample(negative.sample(0.5), 0.5, 0.5);
        assert_sample(not_a_number.sample(0.5), 0.5, 1.0);
    }

    #[test]
    fn fade_scale_dense_sampling_never_overshoots() {
        let effect = VisibilityEffect::FadeScale { hidden_scale: 0.80 };

        for step in 0..=100 {
            let sample = effect.sample(step as f32 / 100.0);
            assert!((0.0..=1.0).contains(&sample.opacity));
            assert!((0.80..=1.0).contains(&sample.scale));
        }
    }
}
