## S31L PlayerTimeline wake owner (2026-07-23)

`PlayerWorkerTimelineWake` is a payload-free bridge into `AppWakeOwner::PlayerTimeline`. The player snapshot remains the payload owner; UI refreshes only when `(media_instance_id, live_revision)` changed, publishes desktop/MPRIS state and requests redraw. This wake must remain active while playback is paused. See `mem:player-core/dynamic-live-timeline-s31l-2026-07-23`.

# App wake bridge и process-lifetime PlaylistRuntime (Session 10A)

Session 10A завершена PASS 2026-07-14. Детальный handoff находится в `user/playlist_queue_implementation_plan.md`.

## Ownership boundaries
- `main.rs` создаёт `EventLoop<AppWakeEvent>` через winit 0.30 `with_user_event().build()`, затем ровно один `EventLoopProxy<AppWakeEvent>` и передаёт его в `AppShell` через `AppWakeProxy`.
- `AppWakeEvent` содержит только `AppWakeOwner`; payload никогда не переносится через winit event и остаётся в bounded owner mailbox.
- `AppShell` реализует `ApplicationHandler<AppWakeEvent>` и является единственным UI-thread drain owner. Redraw запрашивается только после реально видимой мутации; queued/no-op wake и idle state redraw не создают.
- `PlaylistRuntime` принадлежит `AppShell`, а не renderer-bound `AppState`, поэтому переживает `suspended -> resumed` и recreation renderer/player state. После Sessions 10C–13 он владеет policy-neutral media-open coordinator-ом, process-lifetime `PlaylistController`, removal Undo и playlist settings owner-ом; на resume привязывает exact ordered player sender. Disk persistence/load-gate wiring и D10e app-instance lease остаются scope Session 14. Media-open contract: `mem:app-egui/media-open-coordinator-s10c`; controller evolution: `mem:app-egui/playlist-controller-s12a`.

## Wake/mailbox invariant
- `app_wake.rs` владеет per-owner `wake_pending`, sticky `EventLoopClosed`, latest progress slot, lossless completion slot и отдельным producer-disconnect outcome.
- Producer сначала публикует payload под mailbox mutex, затем поднимает atomic false→true edge. Пока UI не drain-ит owner, flood coalesce-ится в одно outstanding wake событие.
- UI drain забирает slots exactly once, очищает edge, повторно проверяет mailbox и re-arm-ит edge, если publish попал в окно race. Проверены обе стороны окна: publish между take/clear и publish между clear/recheck.
- Completion не использует blocking channel send, не перезаписывается вторым terminal и может сосуществовать с concurrent latest progress. После `EventLoopClosed` повторных proxy sends/spin нет.
- Renderer-bound local-file mailbox имеет explicit abandoned acknowledgement: stale wake после drop старого `AppState` не удерживает общий owner edge и не подавляет wake следующего job.
- `playlist-discovery` и `playlist-state` сохраняют собственные neutral wake/mailbox contracts; app bridge их не дублирует и в будущих sessions только адаптирует к `AppWakeEvent`.

## Lifecycle/shutdown
- Каждый успешный resume создаёт новый typed `PlaylistRuntimeBinding` с lifecycle/binding generations. Suspend снимает только binding и сохраняет process owner/ports/load-gate shape; stale binding отвергается.
- Process exit закрывает admission через idempotent bounded `PlaylistRuntime::shutdown`. После Session 10C runtime также cooperative-cancel-ит media-open preparation; уже running blocking I/O может завершиться позднее, но budget ограничен одним stale work. `Completed` означает закрытие admission/coordinator scheduling, а не синхронный join внешнего blocking I/O.
- Defensive 50-ms polling для startup/local/settings jobs остаётся fallback, но correctness delivery теперь обеспечивается wake ports. Continuous playback pacing остаётся `ControlFlow::Wait + request_redraw`; idle остаётся `Wait` без spin.

## Current migrated owners and tests
- Wake-driven completion подключён к startup YouTube/direct media jobs, local-file dialog/preparation и settings dynamic-options refresh без изменения их policy.
- Focused tests находятся в `crates/app-egui/src/app_wake.rs`, `crates/app-egui/src/playlist_runtime.rs` и `crates/app-egui/src/app_shell/mod.rs`.
- Итог Session 10A: 9 wake tests, 3 runtime lifecycle/shutdown tests, 2 no-idle-redraw tests и полный `app-egui` suite 282 tests PASS; strict app Clippy, fmt, Rust 1.96 locked workspace check, diff check и Serena diagnostics PASS.

## Next scope
- Sessions 11A–14A завершены. Process-lifetime runtime теперь также сохраняет D79 confirmation через suspend/resume; renderer-bound AppState читает только immutable safe model. Полный контракт: `mem:app-egui/queue-replacement-confirmation-s14a`.
- Session 15 target-first sibling discovery завершена. Manifest worker и shared discovery executor подключены к тому же event-driven AppWake drain без обязательного 50-ms polling; redraw возникает только при visible progress/status/batch change. Полный контракт: `mem:app-egui/playlist-discovery-s15`. Следующая разрешённая session — 15A; restore-open остаётся Session 17.

## Session 14 persistence wake integration (2026-07-15)

- Playlist state worker outcomes use the shared typed `AppWake` route. The mailbox publish epoch closes the sibling-owner race while draining `wake_pending`; defensive timed polling remains only fallback. Persistence and startup owners live in process-lifetime `PlaylistRuntime`, not renderer-bound AppState.
- Full contract: `mem:app-egui/playlist-persistence-s14`.


## Session 17 startup scheduling (2026-07-16)
- Loading, prepared-awaiting-allocator и renderer-bound stepwise Applying входят в pending-work scheduler; startup poll не вызывает blocking strong install. Wake/defensive poll продвигают exact transaction при `ControlFlow::Wait`. Полный контракт: `mem:app-egui/startup-orchestration-s17`.


## Session 18B desktop owner (2026-07-16)
- Process-lifetime desktop command mailbox shares `AppWakeOwner::PlaylistRuntime`: bounded payload remains outside winit events, send uses the existing false→true wake edge, and UI-thread drain applies neutral MPRIS actions without a hidden playback queue during suspend/no-binding.
- Desktop backend is now a process owner in `PlaylistRuntime/AppShell`; it starts only after D10e lease and shuts down before renderer-bound player. Details: `mem:app-egui/playlist-desktop-transport-s18b`.
