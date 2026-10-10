# A15-live: подпись live и Play из пустого плеера

Статус: PASS (обе части). Два коммита: подпись live; Play без media.

## 1. Подпись live
Видимых английских подписей было одна: правая метка под таймлайном («LIVE» и «DVR 00:30–01:10 · LIVE», `ui/timeline/render.rs`). «VOD» в сайдбаре («Режим: VOD», A8) оставлен осознанно; остальные «VOD»/«Live» в коде — логи, ошибки и внутренние имена, пользователь их не видит.
- Единое место: `ui/stream_kind_labels.rs` (`LIVE_STREAM_LABEL` = «Прямой эфир», `LIVE_STREAM_SHORT_LABEL` = «ЭФИР», `VOD_STREAM_LABEL`). Сайдбар URL берёт полную подпись, таймлайн — короткую.
- Выбор «ЭФИР» для таймлайна: строка времени узкая (слева позиция, справа DVR-диапазон и статус буферизации, окно минимум 400 px). «DVR 00:30–01:10 · Прямой эфир» помещается в пустую строку, но вытесняет инлайн-статусы, поэтому на плашке коротко.
- Тест: `ui::timeline::render::tests::live_time_label_is_drawn_in_russian_and_fits_minimum_window` — egui-кадр шириной 400, accesskit-дерево содержит «… · ЭФИР», слова «LIVE» нет, строка не шире окна. Обновлён `live_labels_distinguish_no_dvr_and_non_zero_dvr`; сайдбарный тест A8 проходит без изменений.

## 2. A15: Play без выбранной строки
**Подтверждено по коду, воспроизведено ожидаемое поведение после починки.**
Корень в player-core: `PlayerSession::play()` переводил state machine в `Playing` без проверки, что есть что играть. В приложении Play при пустом плеере и очереди без current уходил в «legacy toggle» → `PlayerCommand::TogglePlayback` → `Playing` без источника (таймер тикает, сайдбар URL честно пишет «источник ещё не установлен», запросов нет). B8 (`play_queue_blocked_item`) покрывает только остановленную сетью очередь и тут не помогал.
Починено в двух местах владельцев:
- player-core (`session/runtime_control.rs`): Play без demuxer-а (вне `Opening` и staged install) — безвредный no-op; инвариант «Playing только с media». Тело вынесено в `begin_playback`; ~30 юнит-тестов scheduler-а/аудио/worker-а без настоящего demuxer-а переведены на `begin_playback_without_media_for_test` (cfg(test)) или на установленный media. Два worker-теста теперь проверяют, что `Playing` не публикуется.
- app-egui: Play из пустого плеера открывает `current` очереди, иначе первый элемент (`PlaylistController::idle_play_target`, `PlaylistRuntime::play_idle_queue_target`, маршрут `transport_runtime/idle_resume.rs`; туда же перенесена ветка B8, чтобы `transport_runtime.rs` не рос).
Тесты: player-core `play_and_toggle_without_media_stay_idle_and_publish_no_playing`, `play_after_stop_without_media_stays_stopped`, `play_with_installed_media_reaches_playing`; app `playlist_runtime/idle_play/tests.rs` (пустой плеер + непустая очередь без current → запрошен install первого, до установки active нет; нет пути при пустой очереди и установленном media) и `controller/idle_play_target.rs` (current важнее первого).

## Проверки
`cargo test -p player-core -p app-egui --locked` (740 и 1596 зелёные), `cargo fmt --all --check`, `cargo clippy -p player-core -p app-egui --all-targets --locked -- -D warnings`, `check-refactor-guardrails.py`, `check_s42_guardrails.py` — OK.

## Живая приёмка (release, изолированный профиль)
Три http-ссылки через «Добавить URL» без выбора строки: кнопка Play и Space (отдельный запуск) открывают первую ссылку и играют (1/3, таймер идёт, лог «Play из пустого плеера открывает элемент очереди»). Скриншоты `a15_*` остались вне репозитория; приложение, сервер и uinput-скрипт остановлены. Оригинальный баг на старой сборке не воспроизводился отдельно: причина доказана кодом и юнит-тестом player-core.

## Для владельца
Добавить 2–3 URL и не выбирать строку: Play и Space должны начать с первой; открыть live-поток: под таймлайном «ЭФИР», в сайдбаре «Прямой эфир».

## Риски
- Play после Stop при сохранённом active_media контроллера идёт старым путём (не менялось); если player при этом Stopped без media, Play теперь no-op, а не ложный «Playing». Если нужно «Play после Stop возобновляет с current» — отдельное решение.
- Память: `player-core/core` обновлена (инвариант Play без media и маршрут app).
