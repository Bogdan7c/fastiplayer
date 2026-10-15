# Глобальные хоткеи, громкость шагами, подсказка таймлайна (UX edge cases 2, B3, 2026-10-10)

## Решения владельца
Колесо над видео и ↑/↓ — громкость; Ctrl+O — Open; подсказка времени на таймлайне; клавиши скорости (+, =, -, 0) глобальные.

## Владельцы и boundary
- `ui/global_hotkeys.rs` — внутрикадровый (egui `InputState`) сборщик `collect_actions(ctx) -> Vec<ControlAction>`, вызывается в `state/ui_runtime.rs` сразу после `video_surface_input::show` (после панелей кадра). Политика через существующий `KeyboardFocusOwner` (mem:app-egui/app-shell-event-loop-decomposition-s42-2026-08-27):
  `TextInput` — ничего; `Widget` — скорость и Ctrl+O, но НЕ стрелки (они принадлежат кнопке/строке плейлиста); `Nobody` — всё. Ctrl+O -> `ControlAction::OpenFile` (тот же путь, что кнопка Open, правило «занято» внутри `AppState::open_file`).
- Клавиши скорости убраны из `player_controls/playback_rate.rs::collect_input_actions` (там осталось только колесо над кнопкой play).
- `ControlAction::AdjustVolumeSteps(i32)` -> `state/volume_steps.rs`: `volume_command_from_steps` (чистая; `VOLUME_STEP_PERMILLE = 50`; muted+вверх = `ToggleMute{fallback}`, muted+вниз = ничего, упор в границу = ничего, до нуля = `SetVolume(0.0)`), `AppState::apply_volume_steps`. Громкость остаётся у player-а; сохранение — через существующее наблюдение snapshot (UX 11), отдельного кода сохранения нет.
- Колесо: `ui/video_surface_input.rs` (+ `wheel.rs`): `show` теперь возвращает `Vec<VideoSurfaceAction>` (`ToggleFullscreen`, `AdjustVolumeSteps`), `From<VideoSurfaceAction> for ControlAction`. Область видео = свободный rect после панелей; 40 points = шаг; прокрутка съедается только при hover на видео.
- Подсказка таймлайна: `ui/timeline/hover_tooltip.rs` (`hover_time_text` + `show`), вызывается из `render_timeline`; live (`TimelineMode::Live`) и неперематываемый таймлайн — без подсказки; при drag скрыта; без задержки egui (`Tooltip::for_widget(..).at_pointer()`). Hover-превью кадра в app-egui на момент B3 отсутствует.

## Ограничения
- Ярлык «hover над плейлистом» не учитывается: ↑/↓ без фокуса при курсоре над плейлистом меняют громкость (навигация плейлиста работает только при фокусе строки).
- После клика по строке плейлиста фокус остаётся у строки (Widget) — стрелки не громкость, пока фокус не снят (Esc/клик по видео-кнопкам).

- Субтитры (сессия 04, 2026-10-15): `C` вкл/выкл, `V` следующая дорожка (через Off), `Z`/`X` сдвиг ∓0.1 с — тот же egui-путь `collect_actions`, без модификаторов (Ctrl+C/V/Z/X не тронуты); `B` (аудиодорожка) — сессия 07. Детали: `mem:subtitles/ui-s04-part3`.

## Тесты
`ui/global_hotkeys/tests.rs`, `ui/video_surface_input/tests.rs` (колесо), `state/volume_steps/tests.rs`, `ui/timeline/hover_tooltip/tests.rs`.
