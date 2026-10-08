# Автоскрытие chrome в фуллскрине (UX 14, 2026-10-08)

## Решения владельца
- Прячется только в фуллскрине: titlebar (вверх), нижняя панель (вниз), курсор. В окне ничего не прячется.
- Задержка `ui.window.fullscreen_autohide_delay_ms` (default 2500, `0` = не прятать, 0..=30000, step 250, видна в Settings, route `ui.apply`). Валидация — `config/src/validation/ui.rs`.
- Анимация переиспользует sidebar: `SlideTransition` + `EaseInOutCubic` + `CommittedConfigSnapshot::sidebar_slide_duration_seconds()` (при reduced motion = 0 → мгновенно). Default `ui.animations.reduced_motion` сменён на `false` отдельным коммитом.
- Двойной клик по свободной области видео = тот же `ControlAction::ToggleFullscreen`, что кнопка; одиночный клик ничего не делает.
- В фуллскрине titlebar не шлёт `ToggleMaximize` (ни double-click, ни кнопка) — фильтр в `window_chrome::show` по `WindowChromeInput::is_fullscreen`.
- Пауза (и Idle/Opening/Stopped/Ended/Failed) держит панели видимыми; Buffering/Seeking/Scrubbing/Draining — «идёт воспроизведение».

## Владельцы и границы
- `crate::fullscreen_chrome::FullscreenChromeController` (поле `AppState::fullscreen_chrome`) — единственный владелец решения. Цикл кадра: `advance(ChromeFrameInput)` в `ui_prepare` до `render_ui` (активность — `UserActivity::from_raw_input(&RawInput)`: PointerMoved/PointerButton/MouseWheel/Touch/Text/Key pressed; НЕ MouseMoved/PointerGone/key release) → рисование по `presentation()` (`hidden_fraction`, `CursorVisibility`) → `finish_frame(ctx, ChromeHoldObservation)` в конце egui-прохода: причины удержания (`ChromeHoldReason`: PointerOverChrome, SidebarOpen, PopupOpen (`egui::Popup::is_any_open`), PointerDrag (`ctx.dragged_id()`), PlaybackNotRunning) учитываются в СЛЕДУЮЩЕМ advance; курсор прячется `ctx.set_cursor_icon(CursorIcon::None)` (egui-winit → `set_cursor_visible(false)`).
- Windowed: мгновенный snap к видимому, таймер перевзводится → вход в фуллскрин всегда даёт полную задержку. Удержание не ставит wake deadline (иначе пустые пробуждения на паузе); иначе `next_wake_deadline()` участвует в `earliest_ui_wake_deadline` в `frame_prepare.rs`.
- Связка с AppState: `state/fullscreen_chrome_routing.rs` (`advance_fullscreen_chrome`, `fullscreen_chrome_wake_deadline`, перенесённый `toggle_fullscreen` — при `current_monitor() == None` теперь `Borderless(None)` + warn вместо молчаливого no-op).
- `ui::edge_slide` — генерик-сдвиг верхней/нижней egui `Panel`: панель рисуется в дочернем `Ui` (стабильный id_salt, всегда один путь → id виджетов/фокус не меняются), сдвинутом на `extent × fraction` и обрезанном; extent измеряется в том же кадре по `child.cursor()`; в родителе пустая «распорка» `Panel` той же стороны ровно на видимую часть (у egui `Ui::set_cursor` — pub(crate), поэтому так). Инвариант: при fraction 0 остаток родителя идентичен прямой панели (тест). `ui::fullscreen_chrome_panels` — обёртки titlebar/bottom controls; `ui::video_surface_input` — double-click по `ui.available_rect_before_wrap()` после всех панелей и ДО оверлеев.

## Тесты
- `fullscreen_chrome/tests.rs`: fake time (скрытие через N, плавность, wake deadline, движение возвращает, каждая причина удержания, полный delay после снятия удержания, windowed snap, вход в фуллскрин, Disabled, reduced motion, классификация RawInput, реальный egui-кадр для каждой причины и CursorIcon::None).
- `ui/edge_slide/tests.rs` (равенство раскладки с прямыми панелями, полное/половинное скрытие, уехавшая кнопка не кликается, NaN), `ui/video_surface_input/tests.rs`, `window_chrome::tests::title_double_click_maximizes_only_outside_fullscreen`. Помощники кликов: `ui::test_frame::{input_at, click_frames}`.
- Не покрыто автотестом: реальный winit `set_fullscreen`/скрытие курсора — ручная приёмка.
