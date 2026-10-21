# cpu-parity сессия 06b (2026-10-22): UI по видимым изменениям + `--fullscreen` + ленивый VA-API адаптер

Отчёт: `user/cpu-parity/results/06b.md`. План: `user/cpu-parity/06b-ui-visible-changes-fullscreen-flag.md`.

## Решения владельца
- Протокол замера входит в фуллскрин CLI-флагом `--fullscreen` (как `mpv --fs`); флаг передаётся и в уже открытое окно. Короткой формы нет.
- VA-API подзадача (исполнял сабагент Opus 5.5, effort medium): A1 — пакет до `configure_stream` = фатальная типизированная ошибка декодера; B1 — только ленивый адаптер, лишний стартовый декодер-«заготовка» в app-egui — бэклог.

## D1: позиция только когда видна (app-egui)
- Владелец факта видимости — автоскрытие: `fullscreen_chrome::ChromePresentation::panels_on_screen() -> ChromePanelsOnScreen::{Shown, OffScreen}` (`OffScreen` только при `hidden_fraction >= 1.0`, т.е. после конца уезда; анимация = `Shown`; в окне всегда `Shown`).
- `UiFrameModel.position: VisibleTimelinePosition::{Shown(QuantizedTimelinePosition), OffScreen}` (`state/ui_frame_model.rs`, `VisibleTimelinePosition::observe`). Позиция видна только на нижней панели; телеметрия обновляется своим ритмом 250 мс, hover-подсказка показывает время под курсором — позиция в модели им не нужна (проверено исследователем: сайдбар/плейлист/заголовок/центр позицию не показывают).
- Дедлайн позиции: новый владелец `state/position_wake.rs::PositionWakeTracker` (перенесены `PositionWakeMemory`, backoff 8 мс); `OffScreen` → `None` и сброс памяти (после показа панели — свежая граница, не backoff). `AppState::next_position_wake_deadline(snapshot, VisibleTimelinePosition, now)`.
- Live edge (`TimelineVisualKey::live_edge_whole_seconds`) по-прежнему в модели: на live-потоке со скрытой панелью ≤ 1 проход/с — сознательно не трогали.
- Тесты: `state/ui_frame_model/visibility_tests.rs` (настоящий `UiFrameModel::build` + настоящий контроллер автоскрытия), `state/position_wake/tests.rs`, `fullscreen_chrome/tests.rs::panels_are_off_screen_only_after_the_slide_out_finishes`.

## D2: `--fullscreen`
- Разбор: `app_instance::ProcessArgs` (`FULLSCREEN_OPTION`, `fullscreen_intent()`), до `--` в любом месте, повтор не ошибка, после `--` — имя файла; прочие опции `UnknownOption` (текст ошибки называет `--fullscreen`). CLI первого запуска едет в shell одной структурой `app_instance::CommandLineStartup { initial_media, startup_error, fullscreen }` (иначе `AppShell::new` > 7 аргументов).
- Нейтральный тип `desktop_integration::FullscreenIntent::{LeaveAsIs (default), Enter}`; `ForwardedInstanceRequest::with_fullscreen_intent/fullscreen_intent`; `into_parts()` теперь возвращает `ForwardedInstanceRequestParts { action, activation_token, fullscreen }` (именованные поля).
- D-Bus: ключ `platform_data` `io.github.Bogdan7c.Fastiplayer.fullscreen` = `b true`, шлётся только при Enter (без флага запрос побайтно прежний). Получатель (`linux/service.rs::fullscreen_intent_from_platform_data`): нет ключа/`false` → LeaveAsIs, другой тип → warn + LeaveAsIs, запрос всё равно исполняется; неизвестные ключи игнорируются.
- Единый вход в фуллскрин: `crate::window_fullscreen` (`FullscreenWindowPort` для winit `Window`, `enter_fullscreen` идемпотентен → `FullscreenEntry::{Requested, AlreadyFullscreen}`, `toggle_fullscreen` → `FullscreenToggle`); `AppState::toggle_fullscreen` (кнопка/F/двойной клик) делегирует туда. Фальшивое окно — `window_fullscreen/test_support.rs`.
- Исполнение пересланного запроса: `instance_forwarding::execution::execute_forwarded_request` + трейт `ForwardedRequestTarget` (порядок: поднять окно → поставить файлы → фуллскрин). Shell-реализация `app_shell/instance_forwarding.rs::ShellForwardedRequestTarget`. Первый запуск: `app_shell/instance_forwarding/startup_window.rs` (`execute_pending_window_requests` в конце `restore_runtime`: флаг запуска ровно один раз, потом ждущие запросы второго запуска).
- Тесты: `app_instance/tests.rs` (разбор, пересылка с флагом), `app_instance/forwarding/tests.rs`, `instance_forwarding/tests.rs` (сквозной «второй запуск → первый» через настоящий исполнитель, диспетчер открытия и фальшивое окно), `desktop-integration .../linux/tests.rs` (частная шина: Open/Activate с флагом, старый отправитель, битый/false/неизвестный ключ), `window_fullscreen/tests.rs`.

## Протокол замера v3
- `docs/benchmarks/tools/local`: `PROTOCOL_VERSION = 3` один раз в `host.py` (collect/analyze/run_series), `collect.py::fastiplayer_command` = `[binary, '--fullscreen', media]`, KWin-скрипт остаётся у обоих плееров. README — таблица diff v2→v3. `dev/run_pass.py --fastiplayer-arg=--fullscreen`.

## VA-API: ленивый адаптер (`video-vaapi`)
- `decoder/active_codec_adapter.rs::ActiveCodecAdapter` (`AwaitingStreamConfig` | `Configured`), шов `CodecAdapterSource` (prod `DisplayCodecAdapterSource`), `reuse_for_config`/`recreate_for_config`, `CodecAdapterConfigureOutcome::{Reused, Created}` → строка `VA-API codec adapter configured for stream` (backend_name первым полем, `reused_adapter=true|false`). Пакет до configure → `VaapiAdapterDecodeError::StreamNotConfigured` → фатальный `VaapiStreamNotConfiguredError`. `create_default_adapter` удалён.
- Корень «двух инициализаций»: два разных декодер-потока — заготовка app-egui при старте (`init_video_pipeline` → `VaapiVideoBackendFactory::start_for_composition`, при открытии файла не используется) и кандидат strong media install (`staged_media_install::request_detached_backend`). Заготовка — бэклог (B1).

## Замер (780M, XWayland, KWin-фуллскрин; база — двойной клик, новый — `--fullscreen`; 2 пары с чередованием)
- Трасса: после автоскрытия полных egui-проходов 0/с (база 30–33/с на 30 fps), кадр mean 1,05 → 0,77 мс.
- Главный поток 30 fps: −0,7…−1,3 п.п. (H.264 1080p30 2,8–3,0 → 2,2; AV1 1080p30 3,2 → 1,9–2,1; H.264 4K30 3,0–3,2 → 2,2; AV1 4K30 3,0–3,3 → 2,2–2,3); всего −0,5…−1,4; gfx −0,1…−0,2; worker/декодер/RSS без изменений.
