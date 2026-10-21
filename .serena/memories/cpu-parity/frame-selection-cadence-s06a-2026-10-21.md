# Выбор кадра без повтора+пропуска на 60 fps (cpu-parity сессия 06a, 2026-10-21)

Отчёт: `user/cpu-parity/results/06a.md`. Коммиты `3899968d` (player-core), `5dbe297a` (render-wgpu-shell/app-egui). Решение владельца: вариант 1 «ящик на 2 места» + сразу убрать задержку ~16 мс; display-sync (выбор кадра в UI) — кандидат в будущий план.

## Корень (доказан трассой)
- Под XWayland/FIFO (`max_frame_latency = 2`, видео 60,00 fps vs дисплей 59,96 Гц) очередь показа всегда полна → главный поток на КАЖДОМ кадре ~16 мс ждёт в `get_current_texture` → читает слот по vsync. Под нативным Wayland ожидания нет (0,05 мс) и дефекта нет.
- Слот «последний побеждает» + совпадение фаз «публикация worker-а ≈ чтение UI» → дрожание в доли мс решало «успел ли кадр»: повтор, затем перезапись незабранного. Фаза случайна на старте → 2–5-й прогон. Планировщик worker-а ни при чём (все потерянные кадры выбраны вовремя).
- Кадр выбирался ДО ожидания буфера → уходил на экран на ~16 мс старше.

## Владельцы и границы
- `player-core::render_lease_bridge::ordered_present_handoff::OrderedPresentFrameHandoff` — забранный UI кадр + до `UNTAKEN_PRESENT_FRAME_CAPACITY = 2` незабранных по порядку. `publish(frame) -> OrderedPresentFramePublish::{Queued, DroppedOldestUntaken, RestartedSequence{discarded_untaken_frames}}` (новая последовательность = другая render/decoded generation или PTS не растёт → незабранные старые отброшены); `try_take_next()` (UI: старший незабранный становится забранным; без нового — повтор забранного; `Busy` не продвигает); `try_next_identity()` (что UI получит; очередь не двигает); `newest_identity()` (worker: «публиковать?»); `clear()`. Lease-ы освобождаются вне mutex-а (`RetiredLeases`, массив без аллокаций); poison восстанавливается (секции — перестановки).
- `RenderLeaseBridge::publish_present_frame` (было `publish_latest_present_frame`) → `DroppedOldestUntaken` пишет `session.record_present_frame_dropped_untaken()` → `present_frames_dropped_untaken` в `PlaybackDiagnosticsSnapshot`/`LogSummary`/строке `Playback diagnostics summary`.
- `LatestPresentFrameHandoff` остался только для scrub-override; его `publish/clear` — `cfg(test)`: в production слот никто не пишет (давнее, бэклог).
- app-egui: после `Presented` — `AppState::request_follow_up_redraw_for_waiting_present_frame` (state/present_frame_cache.rs): чистое решение `waiting_present_frame_for_follow_up(probe, shown, current_generation)` + `RedrawRuntime::claim_waiting_frame_follow_up` / `WaitingFrameFollowUpMemory` (один запрос на identity → `WaitingPresentFrameFollowUp::{Requested, NotNeeded, AlreadyRequested}`); `mark_pending_worker_redraw` снимается pacing-ом в том же кадре, поэтому следующий кадр — `PresentReusingUi`, не полный egui-проход. Без follow-up на паузе последний выбранный кадр мог не показаться.
- render-wgpu-shell: `Renderer::acquire_surface_frame(screen) -> SurfaceFrameAcquire::{Acquired(AcquiredSurfaceFrame), Dropped(RenderFrameDropReason)}` (resize до acquire, восстановление Outdated/Lost) в `shell/surface_acquire.rs`; `RenderFrameInput.surface` обязателен — `render_frame` без полученного буфера не вызвать. Неиспользованный буфер при drop-е — discard (wgpu 30). `RetainedUiUnavailable` теперь после acquire (буфер возвращается).
- app-egui `frame_prepare/present.rs`: буфер → `FrameSequenceStage::SurfaceAcquire` → выбор кадра/субтитров → `submit_render_frame(FrameSubmission)`; без буфера (`log_frame_without_surface`) кадр плеера НЕ забирается. `account_render_frame_outcome` — общий учёт итога. Трасса acceptance: `selection_to_present_ms`.

## Инварианты
1. Кадры одной последовательности уходят в UI по порядку; сброс только при отставании > 2 (на 60/59,96 неизбежен ~раз в 26 с). 2. Устаревшие кадры после seek не показываются. 3. Слот держит ≤ 3 lease-а. 4. Видеокадр выбирается строго после получения буфера swapchain (контракт последовательности, debug_assert + тест). 5. Очередь двигает только рендер (`try_acquire_present_frame` вызывает лишь `acquire_present_frame_for_render`).

## Отклонено с цифрами
«Очередь двигается только по подтверждению показа» (`confirm_presented`): на 60/59,96 потерь не меньше (у UI нет запаса скорости догнать), отставание на кадр держится дольше (публикация→показ 2,7–25 мс против 3–16). Откатано.

## Метод/цифры
- Трасса: `video_render_acceptance=trace`, `render_frame_timing=trace`, `player_core::session::tick::presentation_scheduler=debug` («Presenting scheduled video frame» = выбор worker-а), под XWayland (`env -u WAYLAND_DISPLAY`) + KWin-скрипт `tools/t480s/fullscreen.py`. Фаза = начало кадра UI − последняя публикация. Метки лога — UTC (при сравнении с `time.time()` не путать с местным временем).
- Quality-gate v2 (штатный `collect.py` + `analyze.attempt_quality`, спецификации серии 06): 60 fps 23/27 → 27/27; 30 fps 8/8 по 600/600. Выбор→показ 16 → 0,34–0,38 мс; публикация→показ 22–31 → 3–16 мс. CPU главного потока/worker-а без изменений, RSS ±1,5 МиБ.
- Модель каденции в тестах (`ordered_present_handoff/tests.rs`) гоняется и на прежнем слоте, который обязан терять (75 потерь/55 пар за 120 с против 4/0) — защита от вакуумного теста.
- Остаток: «текстура занята» (`render_resource_previous_frame_reuse_count`, 0–5 за 40 с) стоит кадр на 60 fps — бэклог.
- Ручная приёмка — raw-uinput клавиатура (`/dev/uinput`, у пользователя есть права) со Space/←/→/N/P на очереди из двух CLI-файлов; HW↔SW вручную не проверено.
