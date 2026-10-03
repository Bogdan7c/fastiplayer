# Исполнение transport-команд, остановленных D08/D39 guard-ом (2026-10-03)

Дополняет `mem:app-egui/playlist-controller-s11b` (D08/D39 guard semantics не менялись — менялось только то, что их результат теперь исполняется).

## Ownership (решение владельца: «Runtime + AppState исполняют»)
- Controller только решает. `controller/transport/deferred.rs`: `execute_deferred_transport_intent(intent, DeferredTransportExecutionContext) -> DeferredTransportExecutionOutcome::{PlayItem, Navigation, NeutralStop, CancelManualNavigation}` прогоняет intent через обычные entry points. `ControllerPlayItemOutcome::Guarded { intent_dispatch, guard }` снова несёт guard.
- Terminal slot `PlaylistController::terminal_transport_intent` (latest-only, без FIFO): все три drain producer-а (`resolve_authorization_dispatch` cancel-win/rejection, `on_installed_with_playback_intent`, `reconcile_released_post_installed_candidate`) вызывают `retain_terminal_transport_intent(&drain)`; Suspend туда не попадает. Любая новая команда (`play_item`, `manual_navigation`, `neutral_stop`, `cancel_manual_navigation`) вытесняет слот. `drain.deferred_intent` остаётся фактом для caller-а/тестов.
- Runtime `playlist_runtime/guarded_transport.rs`: `resolve_transport_guard(guard, position) -> GuardedTransportFollowUp::{Execute, ExecuteAfterRelease{released: ReleasedPendingRequest, executed}, AwaitTerminal, ControllerUnavailable, Fatal}`; `take_terminal_transport_execution(position)`. Контекст D17/D50 строится runtime-ом, dirty публикуется.
- App: `transport_runtime/guarded.rs` (`apply_guarded_transport`, `apply_neutral_stop_request`, per-frame `poll_playlist_transport` = AppState poll + исполнение terminal slot только при `strong_media_open_slot_is_idle()`). `frame_prepare.rs` вызывает `crate::transport_runtime::poll_playlist_transport` (source-order тест в `state/tests.rs` закрепляет).
- `state/playlist_transport/guarded_transport.rs`: `release_superseded_playlist_request` — playlist-owned request: lossless cancel + `released_by_guard_request` (его Failed terminal НЕ идёт в navigation failure/D55); startup-owned (`pending_strong_media_open`) — `supersede_startup_media_apply()` (startup сам отменяет с ApplyRetainedCancelWin), а новый install ждёт в `queued_install` за `queued_behind_foreign_request` и стартует из `poll_playlist_transport`, когда слот свободен.

## Поведение
- AwaitingReady/Reserved: старый request отменяется, команда исполняется сразу. Next/Previous считаются от committed current (незакоммиченный pending target не origin — D08).
- AuthorizationDispatchPending/InFlight: команда ждёт terminal и исполняется ровно один раз относительно нового active (enqueue-win) или старого (cancel-win).

## Тесты
- Controller: `controller/transport/tests/deferred_execution.rs` (6), `controller/manual_navigation/tests/deferred_cursor.rs` (1, восстановлен из 6c2b670f).
- Runtime end-to-end до Installed: `playlist_runtime/transport_guard_regressions.rs` (8).
- Ограничение: AppState-ветки (release owner routing, foreign queue, released marker) без unit-тестов — нет AppState harness без Renderer.

## Известные смежные места (не трогались)
- `cancel_playlist_navigation_from_ui` → `ManualNavigationCancelOutcome::CancelPending` (прямой, не через guard) по-прежнему не отменяет coordinator request.
- D53 `replace_aborted_playlist_install` с `next: None` отправляет Cancelled terminal в navigation failure recovery (может создать D55 anchor) — не проверялось.
