# Session 14 — app persistence integration и process shutdown (2026-07-15)

## Границы владения

- `app-egui::app_instance` владеет typed `ProcessArgs`, platform-neutral `AppInstanceLease` и fake-able platform adapter. Обязательный порядок bootstrap: args_os parse → trusted `ConfigPaths` discovery → lease `rustiplayer.instance.lock` → config load/create → playlist state inspection и media preparation → transfer lease в `AppShell`.
- Linux-v1 adapter использует `File::try_lock`, 0700/0600, no-follow open, regular-file/current-effective-user validation, stable descriptor/path inode, no unlink и explicit close-on-exec. Contention и unsupported platform завершаются до config/state/media/player/window/MPRIS side effects.
- `PlaylistRuntime`, а не renderer-bound `AppState`, владеет startup load gate, `PlaylistStateStore`, save worker, dirty snapshots, warnings/retry и shutdown state. `ConfigPaths::playlist_state_path` строит `playlist-state.json`; путь не входит в AppConfig TOML.
- До typed load decision controller физически отсутствует, allocator gate закрыт, startup media остаётся ID-less draft и player staging/install/domain commit/save/quarantine запрещены.
- Valid inspection открывает gate восстановленным watermark даже если restore apply superseded. Missing или успешный matching quarantine открывают persistent initial allocator. Newer/unrecognized/duplicate version, quarantine failure/source change открывают только generation-scoped non-persistent allocator и блокируют writer.
- D65 structural actions supersede restore item/traversal apply, но не allocator decision; mode-only changes coalesce в один winning-queue overlay.
- Dirty revision snapshot создаётся только после реальной mutation. No-op не создаёт revision; removal и Undo — две обычные monotonic revisions. Snapshot исключает selection, transient errors/pending/tombstone/stop latch/URL draft/Undo slot.

## Wake, durability и shutdown

- Persistence completion использует typed `AppWake` mailbox; publish epoch закрывает sibling-owner lost-wake race. Timed poll остаётся defensive fallback.
- Read model различает `NotReplaced`, `ReplacedDurabilityUnconfirmed` и `Durable`; post-rename directory-sync failure не маскируется как старый target. D69 latest revision, bounded retry/backoff и manual Retry сохранены.
- Полный process shutdown использует один absolute `ShutdownDeadline`. Actions закрываются, pending owners отменяются, затем terminal join/flush выполняется для MPRIS, PlayerWorker, local-file, startup media, media preparation, settings dynamic jobs, playlist inspection/quarantine и state writer.
- Lease — последний process owner: освобождается только после terminal owners. Если deadline истёк и writable/live thread ещё существует, UI не возобновляется: процесс немедленно выходит non-zero, пока lease всё ещё удерживается.
- Renderer suspend лишь detach-ит renderer/player binding по generation и переносит app-owned local job обратно в `AppShell`; playlist queue/draft/Undo/dirty, startup owner, store worker и lease сохраняются. Media checkpoint/reopen остаётся Session 14B.
- AppShell terminal helpers вынесены в `app_shell/shutdown.rs`; app-instance в отдельном `app_instance`, startup persistence в `playlist_runtime/startup*`, save wiring в `playlist_runtime/persistence*`, общий deadline в `process_shutdown.rs`.

## Проверки и дальнейший scope

- Focused args/lease/bootstrap/load-gate/persistence/durability/wake/retry/shutdown tests и full crate suites закрепляют границы. Rust 1.96 locked workspace check, strict Clippy, fmt, guardrails и diff check входят в handoff Session 14.
- Session 14 не открывает restored current, не добавляет destructive replacement confirmation, media checkpoint/resume или новую CLI precedence. Следующие владельцы: Session 14A, 14B и 17 соответственно.


## Session 14A continuation (2026-07-15)
- D79 pending confirmation теперь является process-lifetime transient state `PlaylistRuntime`: оно не сериализуется, не создаёт dirty revision, переживает renderer/AppState recreation и отменяется process shutdown-ом.
- Ранний in-app open до load decision использует существующий D65 startup draft replacement: restore items apply superseded, allocator decision сохраняется, provisional Item ID/player commit до gate не возникает.
- Полный контракт: `mem:app-egui/queue-replacement-confirmation-s14a`.
