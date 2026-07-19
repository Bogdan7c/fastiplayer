# Reusable visibility animations (2026-07-18)

- `animation-core::visibility` — нейтральный owner чистой математики показа/скрытия UI: `VisibilityEffect::{Fade, FadeScale { hidden_scale }}` и `VisibilitySample { opacity, scale }`. Нормализованный progress защищён от NaN и выхода за `0..=1`; scale/opacity не дают overshoot.
- `animation_core::Easing::EaseOutCubic` добавлен как общий cubic ease-out без зависимости от egui/clock.
- Тонкий app-адаптер находится в `app-egui::ui::animation`: `VisibilityAnimationSpec`, `VisibilityTarget` и typed `UiMotion::{Standard, Reduced}`. Он владеет stable egui ID, target, timing/repaint и мгновенным reduced-motion режимом, но не знает о конкретном widget/domain.
- Для точного обратного перехода адаптер берёт линейную позицию из `egui::Context::animate_bool_with_time` и отдельно применяет один и тот же core easing к progress видимости. Это сохраняет непрерывность при реверсе посередине и не наследует egui target-dependent flip easing.
- Первый consumer — Playlist header Undo: 180 ms, opacity `0→1`, scale `0.80→1.00`; выход является точным обратным переходом, reduced motion мгновенный. Interaction component живёт в `app-egui::ui::playlist::header_undo`, не в toolbar.
- Focused tests закрепляют endpoints/intermediate/NaN/clamp/no-overshoot в core и fade-in/fade-out/reversal/reduced-motion в egui adapter.

Связанные memories: `mem:core`, `mem:app-egui/playlist-toolbar-undo-2026-07-18`, `mem:settings-ui/reduced-motion-2026-07-18`.

## UI motion policy follow-up (2026-07-19)
- App adapter policy переименована из `VisibilityMotion` в общий `UiMotion`, чтобы Undo visibility и Playlist positional accent использовали одну typed reduced-motion настройку. `animation-core` API/математика не менялись: active accent переиспользует существующие `SlideTransition` и `Easing::EaseOutCubic`.
