//! `animation-core` — нейтральная математика UI-анимаций проекта.
//!
//! Крейт не знает про egui, WGPU, окна и системные часы: клиент сам
//! поставляет дельту времени кадра и длительность анимации (из config).
//! Здесь только детерминированная арифметика прогресса и easing-кривые,
//! поэтому всё легко тестируется и ничего не стоит по ресурсам.

/// Кривая сглаживания линейного прогресса анимации.
///
/// Прогресс на входе и выходе лежит в диапазоне `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    /// Без сглаживания: равномерная скорость от начала до конца.
    Linear,
    /// Плавный разгон и плавное торможение (кубическая кривая).
    ///
    /// Стандартная кривая для выезжающих панелей: движение мягко
    /// стартует и мягко останавливается.
    EaseInOutCubic,
}

impl Easing {
    /// Преобразует линейный прогресс `0.0..=1.0` в сглаженный.
    ///
    /// Вход за пределами диапазона зажимается, NaN трактуется как `0.0`,
    /// чтобы кривая никогда не возвращала мусор в рендер.
    #[must_use]
    pub fn apply(self, linear_progress: f32) -> f32 {
        // Защита от NaN и выхода за диапазон до применения кривой.
        let clamped_progress = if linear_progress.is_nan() {
            0.0
        } else {
            linear_progress.clamp(0.0, 1.0)
        };

        match self {
            Easing::Linear => clamped_progress,
            Easing::EaseInOutCubic => {
                // Классическая cubic ease-in-out:
                // первая половина — разгон (4t^3),
                // вторая половина — симметричное торможение.
                if clamped_progress < 0.5 {
                    4.0 * clamped_progress * clamped_progress * clamped_progress
                } else {
                    let remaining = -2.0 * clamped_progress + 2.0;
                    1.0 - remaining * remaining * remaining / 2.0
                }
            }
        }
    }
}

/// Переход «закрыто/открыто» с непрерывной позицией `0.0..=1.0`.
///
/// Позиция хранится линейной; easing применяется только при чтении
/// прогресса. Благодаря этому смена цели в середине анимации (реверс)
/// продолжает движение с текущего места без визуального скачка.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlideTransition {
    /// Линейная позиция перехода: `0.0` — полностью закрыто,
    /// `1.0` — полностью открыто.
    position: f32,
    /// Целевая позиция: только `0.0` (закрыть) или `1.0` (открыть).
    target: f32,
}

impl SlideTransition {
    /// Переход в полностью закрытом состоянии.
    #[must_use]
    pub fn closed() -> Self {
        Self {
            position: 0.0,
            target: 0.0,
        }
    }

    /// Переход в полностью открытом состоянии.
    #[must_use]
    pub fn open() -> Self {
        Self {
            position: 1.0,
            target: 1.0,
        }
    }

    /// Задаёт цель перехода. Позиция не меняется: движение к новой цели
    /// продолжится из текущей точки при следующих вызовах [`Self::advance`].
    pub fn set_target_open(&mut self, open: bool) {
        self.target = if open { 1.0 } else { 0.0 };
    }

    /// Продвигает позицию к цели.
    ///
    /// * `dt_seconds` — дельта времени кадра; NaN и отрицательные значения
    ///   игнорируются (позиция не меняется).
    /// * `duration_seconds` — полная длительность перехода `0 -> 1`;
    ///   значение `<= 0` или NaN означает «без анимации» — мгновенный
    ///   прыжок к цели.
    pub fn advance(&mut self, dt_seconds: f32, duration_seconds: f32) {
        // Длительность 0/NaN (настройка «без анимации») — мгновенный переход.
        if duration_seconds.is_nan() || duration_seconds <= 0.0 {
            self.position = self.target;
            return;
        }
        // Некорректная дельта времени (NaN/отрицательная) не должна ломать состояние.
        if dt_seconds.is_nan() || dt_seconds <= 0.0 {
            return;
        }

        // Линейная скорость: весь путь 0..1 за duration_seconds.
        let step = dt_seconds / duration_seconds;
        if self.position < self.target {
            self.position = (self.position + step).min(self.target);
        } else if self.position > self.target {
            self.position = (self.position - step).max(self.target);
        }
    }

    /// `true`, пока позиция не достигла цели (анимация идёт).
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.position != self.target
    }

    /// `true`, когда переход полностью закрыт и не движется.
    #[must_use]
    pub fn is_fully_closed(&self) -> bool {
        self.position == 0.0 && self.target == 0.0
    }

    /// Сглаженный прогресс `0.0..=1.0` для отрисовки.
    #[must_use]
    pub fn eased_progress(&self, easing: Easing) -> f32 {
        easing.apply(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Допуск сравнения float в тестах.
    const EPSILON: f32 = 1e-6;

    #[test]
    fn advance_moves_position_linearly_and_clamps_at_target() {
        let mut transition = SlideTransition::closed();
        transition.set_target_open(true);

        // Четверть пути за четверть длительности.
        transition.advance(0.125, 0.5);
        assert!((transition.eased_progress(Easing::Linear) - 0.25).abs() < EPSILON);

        // Большой dt не должен перелетать за цель.
        transition.advance(10.0, 0.5);
        assert!((transition.eased_progress(Easing::Linear) - 1.0).abs() < EPSILON);
        assert!(!transition.is_animating());
    }

    #[test]
    fn reverse_mid_animation_continues_from_current_position() {
        let mut transition = SlideTransition::closed();
        transition.set_target_open(true);
        transition.advance(0.3, 0.5); // позиция 0.6

        // Реверс: закрываем с текущей точки, без скачка.
        transition.set_target_open(false);
        assert!((transition.eased_progress(Easing::Linear) - 0.6).abs() < EPSILON);

        transition.advance(0.1, 0.5); // минус 0.2
        assert!((transition.eased_progress(Easing::Linear) - 0.4).abs() < EPSILON);
        assert!(transition.is_animating());
    }

    #[test]
    fn zero_duration_jumps_to_target_instantly() {
        let mut transition = SlideTransition::closed();
        transition.set_target_open(true);

        transition.advance(0.0, 0.0);
        assert!(!transition.is_animating());
        assert!((transition.eased_progress(Easing::Linear) - 1.0).abs() < EPSILON);

        transition.set_target_open(false);
        transition.advance(0.016, -1.0);
        assert!(transition.is_fully_closed());
    }

    #[test]
    fn invalid_dt_and_duration_do_not_corrupt_state() {
        let mut transition = SlideTransition::closed();
        transition.set_target_open(true);
        transition.advance(0.25, 0.5); // позиция 0.5

        // NaN/отрицательный dt — позиция не меняется.
        transition.advance(f32::NAN, 0.5);
        transition.advance(-1.0, 0.5);
        assert!((transition.eased_progress(Easing::Linear) - 0.5).abs() < EPSILON);

        // NaN длительность трактуется как «без анимации» — прыжок к цели,
        // но состояние остаётся валидным (в диапазоне 0..=1).
        transition.advance(0.016, f32::NAN);
        assert!((transition.eased_progress(Easing::Linear) - 1.0).abs() < EPSILON);
    }

    #[test]
    fn ease_in_out_cubic_boundaries_midpoint_and_monotonicity() {
        let easing = Easing::EaseInOutCubic;
        assert!((easing.apply(0.0) - 0.0).abs() < EPSILON);
        assert!((easing.apply(1.0) - 1.0).abs() < EPSILON);
        assert!((easing.apply(0.5) - 0.5).abs() < EPSILON);

        // Симметрия относительно середины: f(t) + f(1-t) = 1.
        for step in 0..=10 {
            let t = step as f32 / 10.0;
            assert!((easing.apply(t) + easing.apply(1.0 - t) - 1.0).abs() < 1e-5);
        }

        // Монотонное возрастание без провалов.
        let mut previous = easing.apply(0.0);
        for step in 1..=100 {
            let current = easing.apply(step as f32 / 100.0);
            assert!(current >= previous);
            previous = current;
        }
    }

    #[test]
    fn easing_clamps_out_of_range_and_nan_input() {
        for easing in [Easing::Linear, Easing::EaseInOutCubic] {
            assert!((easing.apply(-1.0) - 0.0).abs() < EPSILON);
            assert!((easing.apply(2.0) - 1.0).abs() < EPSILON);
            assert!((easing.apply(f32::NAN) - 0.0).abs() < EPSILON);
        }
    }
}
