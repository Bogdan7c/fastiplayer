# UX17: гарантированная доставка команд worker-а (резерв очереди)

Связано: `mem:player-core/core`, `mem:player-core/scrub-commit-policy-s09`, `mem:app-egui/timeline-decomposition-s21`.

## Исходный дефект (доказан тестами, сессия UX 17, 2026-10-10)
- Все UI-команды шли неблокирующим `try_send` в bounded канал на 128 (`COMMAND_CHANNEL_CAPACITY`); при `Full` app только писал `warn!`.
- Переполнение возможно только при залипшем worker thread-е (≥ ~2 с при drag громкости 60 Гц): открытие/пересоздание cpal output без таймаута, синхронный seek/read локального файла, decoder flush/configure до 2 с, join старого decoder-а. Сетевые demux-ы читают в фоновом `ProgressiveDemuxer` и worker не блокируют. Live scrub `throttled_latest` при залипании шлёт ≤ ~4 preview/с.
- Потеря `EndScrub` оставляла `Scrubbing` навсегда (ни timeout-а, ни tick-а): кнопка показывала Play, а первый toggle ставил Paused (cancel scrub восстанавливает Playing → toggle → pause); `wants_continuous_redraw` крутил перерисовку. Потеря финального `SetVolume` оставляла промежуточную громкость.
- Не дефект: исчезновение жеста при `bounds = None` — core сам выходит из scrub при смене media (`clear_simple_scrub` в reset/staged commit) и при пропаже live-окна (`expire_dynamic_seek_or_scrub`). Потеря `BeginScrub` безопасна: `PreviewScrub`/`UpdateScrub` сами входят в scrub.

## Решение владельца: вариант C (резерв + слияние)
- Владелец: `crates/player-core/src/worker/command_queue.rs`. `WorkerCommandQueue` (внутри `PlayerCommandSender`, поле `command_queue`) и `WorkerCommandInbox` (внутри `PlayerWorkerRuntime`, поле `command_inbox`) делят `CommandReserve` под одним mutex-ом.
- Классификация типом: `PlayerCommandDelivery::of` → `LatestValue(Volume | PlaybackRate | ScrubTarget)` или `Ordered`; match без wildcard — новая `PlayerCommand` обязана выбрать класс. Не-`Player` worker commands — ordered.
- Инварианты порядка: (1) пока резерв не пуст, обычные команды идут только в резерв; (2) receipt-команды (`try_send_worker_command`, `apply_runtime_settings`) при непустом резерве получают `Full`/`Backpressure`; (3) inbox читает резерв только при пустой основной очереди; исключение — terminal shutdown `try_send_bypassing_reserve`.
- В резерве сливается только с хвостом: соседняя latest-value того же вида. Лимит `COMMAND_RESERVE_CAPACITY = 64` для обычных; `send_lossless_worker_command` (media install cleanup) принимается сверх лимита и больше не блокирует caller.
- Public `PlayerCommandSender::try_send` сохранил `Result<(), PlayerWorkerSendError>`; `Full` теперь означает «переполнен и резерв» (реальная потеря). Эпизод переполнения: warn при первом уходе в резерв, info с `reserved/coalesced/rejected` при опустошении (`CommandReserveEpisode`).
- Drop inbox-а закрывает резерв (`Disconnected` вместо молчаливого приёма) и роняет хранимые команды (receipts → missing outcome).
- Worker `select!` (3 места в `runtime_wait.rs`) слушает `reserve_wake_receiver()` (capacity-one wake); разбор — обычный `drain_pending_command_batch` через `receive_next_command`.
- UI: `app-egui/src/ui/control_action_coalescing.rs::coalesce_consecutive_volume` схлопывает подряд идущие `ControlAction::SetVolume` перед `handle_control_actions`.

## Тесты
- `worker/command_queue/tests.rs`: fast path, порядок после переполнения, sticky резерв, слияние/неслияние, receipt backpressure, лимит и счётчики, lossless сверх лимита, shutdown bypass, closed inbox, wake.
- `worker/tests/command_loss.rs`: EndScrub в полную очередь → выход из Scrubbing и seek к цели; Pause в полную очередь применяется последней; 1000 SetVolume залипшему worker-у → финальная громкость, 1 ячейка резерва; смена media во время scrub; живой worker без отказов.
- `ui/timeline/gesture.rs::live_drag_losing_bounds_clears_gesture_without_cancel_action`, `ui/control_action_coalescing.rs` tests.
- Test helpers: `runtime_for_tests_with_public_sender` (общий резерв), `PlayerCommandSender::for_tests` = detached резерв для fixture-ов с сырым receiver-ом.
