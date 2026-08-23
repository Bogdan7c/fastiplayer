# AUD-014: bounded timeline seek settlement перед suspend/shutdown (2026-08-23)

## Подтверждённый исходный дефект

- Независимая сессия провела production `PlayerWorker` seek и настоящий `PlaylistRuntime` suspend/checkpoint/restore.
- При подтверждённой позиции 10 s exact seek на 90 s был принят и admitted в worker-receipted `PreparedDemuxSeekPort`, но terminal receipt удержан.
- Snapshot стал `PlaybackState::Seeking` с `current_position = 10 s`; прежний lifecycle сохранил и восстановил `SeekTo(10 s)`.
- Причина: `AppState::dispatch_exact_timeline_seek` хранит receipt в renderer-bound transport, обычный polling обрабатывает `Applied`, а suspend/shutdown раньше обходили этот owner boundary; `PlayerWorker::latest_snapshot` использует nonblocking `try_iter`.

## Новый owner boundary и invariant

- `ExactTimelineSeekReceipt::wait_for_outcome_until(Instant)` ждёт terminal outcome до абсолютного deadline без busy-loop-а и различает `DeadlineElapsed` / `MissingOwnerOutcome`.
- `state/playlist_transport/lifecycle_settlement.rs` владеет `LifecycleTimelineSeekSettlement` и обработкой одного terminal outcome для обычного polling и lifecycle barrier-а.
- Все pending receipts делят один absolute deadline. Каждый `Applied` продвигает checkpoint к последней подтверждённой позиции; rejected terminal outcome оставляет последнюю подтверждённую позицию.
- `AppShell` даёт settlement budget 1 s. Terminal shutdown дополнительно ограничивает этот deadline общим process shutdown deadline 5 s.
- При deadline/missing owner remaining renderer-bound receipts abandon-ятся, затем player owner bounded завершается. Checkpoint получает документированную pre-seek/last-settled позицию, а не неподтверждённый UI target.
- `LifecycleTimelineCheckpointPosition::{LatestSnapshot, SettledSeek, CancelledPendingSeek, MissingSeekOwnerOutcome}` явно передаёт provenance в suspend и sidecar persistence.
- `PlaylistRuntime::capture_suspended_media_checkpoint_after_seek_settlement` и `force_resume_checkpoint_after_seek_settlement` используют explicit position даже при transient `PlaybackState::Seeking`; live/EOF/identity/intent ownership остаются прежними.
- Нельзя снова брать suspend/shutdown checkpoint напрямую из `refresh_player_snapshot()`, не выполнив pending seek settlement.

## Расположение

- Player receipt wait: `crates/player-core/src/media_install/timeline_seek.rs`.
- App receipt owner/barrier: `crates/app-egui/src/state/playlist_transport/lifecycle_settlement.rs`.
- Typed checkpoint provenance: `crates/app-egui/src/playlist_runtime/lifecycle_checkpoint.rs`.
- Suspend/shutdown callsites: `crates/app-egui/src/app_shell/mod.rs`.
- Suspend functional regressions: `crates/app-egui/src/playlist_runtime/suspend_resume/tests/aud014_pending_seek_lifecycle.rs`.
- Shutdown sidecar regression: `crates/app-egui/src/playlist_runtime/resume_persistence/tests/aud014_shutdown_checkpoint.rs`.
- Audit handoff: `user/project_health_audit_2026-08-22.md`.

## Проверки

- `cargo test -p player-core`: 646 passed.
- `cargo test -p app-egui --all-features`: 962 passed.
- `cargo test -p app-egui aud014_ --all-features`: 3 passed.
- AUD-014 suspend regressions дополнительно прошли три последовательных повтора.
- strict Clippy app/player all-features и app no-default-features: clean.
- locked workspace check, fmt, refactor guardrails и diff check: clean.

Связанные знания: `mem:app-egui/suspend-resume-checkpoint-s14b`, `mem:app-egui/playlist-transport-s18a`, `mem:player-core/scrub-commit-policy-s09`.
