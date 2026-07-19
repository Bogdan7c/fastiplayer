//! Состояние UI-анимаций и тонкие egui-адаптеры над нейтральной математикой.

use std::hash::Hash;
use std::time::Duration;

use animation_core::Easing;
use animation_core::visibility::{VisibilityEffect, VisibilitySample};
use egui::{Id, Ui};
use media_core::TimelineSnapshot;

/// Authoritative цель visibility-перехода без неочевидного позиционного `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VisibilityTarget {
    /// Элемент должен исчезнуть.
    Hidden,
    /// Элемент должен появиться.
    Visible,
}

impl VisibilityTarget {
    /// Переводит intent в формат внутреннего egui animation manager.
    const fn is_visible(self) -> bool {
        matches!(self, Self::Visible)
    }

    /// Возвращает мгновенную границу для reduced-motion или нулевой длительности.
    const fn instant_progress(self) -> f32 {
        if self.is_visible() { 1.0 } else { 0.0 }
    }
}

/// Общая пользовательская политика движения без позиционного `bool` в UI API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiMotion {
    /// Обычная анимация с указанной длительностью.
    Standard,
    /// Мгновенный переход для reduced-motion режима.
    Reduced,
}

impl UiMotion {
    /// Переводит подтверждённую config-настройку в typed animation policy.
    pub(crate) const fn from_reduced_motion(reduced_motion: bool) -> Self {
        if reduced_motion {
            Self::Reduced
        } else {
            Self::Standard
        }
    }
}

/// Named параметры visibility-анимации делают callsite самодокументируемым.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VisibilityAnimationSpec {
    /// Authoritative цель текущего кадра.
    pub(crate) target: VisibilityTarget,
    /// Полная длительность пути `0.0 -> 1.0`.
    pub(crate) duration: Duration,
    /// Кривая применяется к линейной позиции независимо от направления.
    pub(crate) easing: Easing,
    /// Toolkit-neutral преобразование progress в paint-параметры.
    pub(crate) effect: VisibilityEffect,
    /// Пользовательская политика движения.
    pub(crate) motion: UiMotion,
}

/// Полная спецификация одного переиспользуемого visibility-перехода.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VisibilityAnimation {
    /// Stable egui Id сохраняет progress между immediate-mode кадрами.
    id: Id,
    /// Named параметры отделены от механизма stable Id.
    spec: VisibilityAnimationSpec,
}

impl VisibilityAnimation {
    /// Создаёт анимацию с persistent Id внутри текущего UI scope.
    ///
    /// `id_salt` должен быть стабильным для одного логического элемента
    /// и уникальным среди соседних visibility-анимаций.
    pub(crate) fn new(ui: &Ui, id_salt: impl Hash, spec: VisibilityAnimationSpec) -> Self {
        Self {
            id: ui.make_persistent_id(id_salt),
            spec,
        }
    }

    /// Возвращает sample и автоматически поддерживает repaint через egui.
    ///
    /// Egui хранит линейную reversible-позицию. Easing применяется здесь
    /// одинаково к этой позиции в обоих направлениях: исчезновение поэтому
    /// является точным обратным путём появления, а реверс не создаёт скачка.
    #[must_use]
    pub(crate) fn sample(self, ui: &Ui) -> VisibilitySample {
        // Reduced motion не трогает animation manager и мгновенно отражает target.
        if self.spec.motion == UiMotion::Reduced || self.spec.duration.is_zero() {
            return self.spec.effect.sample(self.spec.target.instant_progress());
        }

        // Линейный manager egui сам хранит stable state, учитывает frame time,
        // поддерживает mid-flight reversal и запрашивает repaint до границы.
        let linear_progress = ui.ctx().animate_bool_with_time(
            self.id,
            self.spec.target.is_visible(),
            self.spec.duration.as_secs_f32(),
        );
        // Toolkit-neutral easing и effect остаются детерминированными и тестируемыми.
        self.spec
            .effect
            .sample(self.spec.easing.apply(linear_progress))
    }
}

/// Состояние визуальных анимаций controls/video overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationState {
    /// Нужно ли затемнять stale video frame вне active scrub.
    pub dim_stale_frame: bool,
}

impl AnimationState {
    /// Строит animation state из нейтрального timeline snapshot-а.
    ///
    /// Active scrub обновляет target на timeline, но не меняет яркость cached
    /// video frame-а, чтобы preview latency не выглядела как мигание видео.
    #[must_use]
    pub const fn from_timeline(timeline: &TimelineSnapshot) -> Self {
        Self {
            dim_stale_frame: timeline.stale_frame && !timeline.scrubbing,
        }
    }
}

impl Default for AnimationState {
    /// По умолчанию UI не применяет transient-анимации.
    fn default() -> Self {
        Self {
            dim_stale_frame: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use animation_core::Easing;
    use animation_core::visibility::{VisibilityEffect, VisibilitySample};
    use egui::{Context, RawInput, Rect, pos2, vec2};
    use media_core::{MediaDuration, TimelineSnapshot};

    use super::{
        AnimationState, UiMotion, VisibilityAnimation, VisibilityAnimationSpec, VisibilityTarget,
    };

    /// Полная длительность synthetic visibility-перехода из UX-контракта.
    const VISIBILITY_DURATION: Duration = Duration::from_millis(180);

    /// Запускает production adapter на одном synthetic egui frame.
    fn visibility_sample_for_frame(
        context: &Context,
        time_seconds: f64,
        predicted_delta_seconds: f32,
        target: VisibilityTarget,
        reduced_motion: bool,
    ) -> VisibilitySample {
        // Synthetic clock делает тест независимым от wall time и нагрузки CI.
        let mut input = RawInput {
            time: Some(time_seconds),
            predicted_dt: predicted_delta_seconds,
            ..RawInput::default()
        };
        // Стабильный viewport исключает layout-различия между кадрами.
        input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, 180.0)));
        // NaN обнаружит пропущенный egui closure вместо ложноположительного теста.
        let mut sample = VisibilitySample {
            opacity: f32::NAN,
            scale: f32::NAN,
        };
        // Один Context сохраняет animation manager и stable Id между кадрами.
        let _ = context.run_ui(input, |ui| {
            sample = VisibilityAnimation::new(
                ui,
                "test_visibility",
                VisibilityAnimationSpec {
                    target,
                    duration: VISIBILITY_DURATION,
                    easing: Easing::EaseOutCubic,
                    effect: VisibilityEffect::FadeScale { hidden_scale: 0.80 },
                    motion: UiMotion::from_reduced_motion(reduced_motion),
                },
            )
            .sample(ui);
        });
        assert!(sample.opacity.is_finite());
        assert!(sample.scale.is_finite());
        sample
    }

    /// Проверяет, что обычный seek всё ещё получает stale-затемнение.
    #[test]
    fn stale_seek_dims_video_frame() {
        let mut timeline = TimelineSnapshot::seekable_vod(MediaDuration::from_secs(30));
        timeline.stale_frame = true;

        let animation_state = AnimationState::from_timeline(&timeline);

        assert!(animation_state.dim_stale_frame);
    }

    /// Проверяет, что interactive scrub не меняет яркость cached video frame-а.
    #[test]
    fn active_scrub_keeps_video_brightness_stable() {
        let mut timeline = TimelineSnapshot::seekable_vod(MediaDuration::from_secs(30));
        timeline.scrubbing = true;
        timeline.stale_frame = true;

        let animation_state = AnimationState::from_timeline(&timeline);

        assert!(!animation_state.dim_stale_frame);
    }

    #[test]
    fn visibility_fade_in_uses_full_180_milliseconds_and_cubic_out() {
        let context = Context::default();
        let hidden =
            visibility_sample_for_frame(&context, 0.0, 0.09, VisibilityTarget::Hidden, false);
        let halfway =
            visibility_sample_for_frame(&context, 0.09, 0.09, VisibilityTarget::Visible, false);
        let visible =
            visibility_sample_for_frame(&context, 0.18, 0.09, VisibilityTarget::Visible, false);

        assert_eq!(hidden.opacity, 0.0);
        assert!((halfway.opacity - 0.875).abs() < 0.0001);
        assert!((halfway.scale - 0.975).abs() < 0.0001);
        assert_eq!(visible.opacity, 1.0);
        assert_eq!(visible.scale, 1.0);
    }

    #[test]
    fn visibility_fade_out_is_the_exact_reverse_path() {
        let context = Context::default();
        let _ = visibility_sample_for_frame(&context, 0.0, 0.09, VisibilityTarget::Hidden, false);
        let _ = visibility_sample_for_frame(&context, 0.09, 0.09, VisibilityTarget::Visible, false);
        let _ = visibility_sample_for_frame(&context, 0.18, 0.09, VisibilityTarget::Visible, false);
        let halfway_out =
            visibility_sample_for_frame(&context, 0.27, 0.09, VisibilityTarget::Hidden, false);
        let hidden =
            visibility_sample_for_frame(&context, 0.36, 0.09, VisibilityTarget::Hidden, false);

        // Обратный путь использует cubic-out от текущей линейной позиции,
        // поэтому midpoint закрытия совпадает с midpoint появления.
        assert!((halfway_out.opacity - 0.875).abs() < 0.0001);
        assert!((halfway_out.scale - 0.975).abs() < 0.0001);
        assert_eq!(hidden.opacity, 0.0);
        assert!((hidden.scale - 0.80).abs() < 0.0001);
    }

    #[test]
    fn visibility_reversal_preserves_the_exact_visual_sample() {
        let context = Context::default();
        let quarter_frame_seconds = 0.045;
        let _ = visibility_sample_for_frame(
            &context,
            0.0,
            quarter_frame_seconds,
            VisibilityTarget::Hidden,
            false,
        );
        let opening = visibility_sample_for_frame(
            &context,
            0.045,
            quarter_frame_seconds,
            VisibilityTarget::Visible,
            false,
        );
        let reversed = visibility_sample_for_frame(
            &context,
            0.045,
            quarter_frame_seconds,
            VisibilityTarget::Hidden,
            false,
        );

        assert!((opening.opacity - reversed.opacity).abs() < 0.0001);
        assert!((opening.scale - reversed.scale).abs() < 0.0001);
    }

    #[test]
    fn reduced_motion_switches_visibility_instantly() {
        let context = Context::default();
        let hidden =
            visibility_sample_for_frame(&context, 0.0, 0.09, VisibilityTarget::Hidden, true);
        let visible =
            visibility_sample_for_frame(&context, 0.0, 0.09, VisibilityTarget::Visible, true);

        assert_eq!(hidden.opacity, 0.0);
        assert!((hidden.scale - 0.80).abs() < 0.0001);
        assert_eq!(visible.opacity, 1.0);
        assert_eq!(visible.scale, 1.0);
    }
}
