# Stop-after-current removed (2026-07-18)

## Product decision
- По решению пользователя one-shot фича «После текущего» удалена полностью, а не скрыта. Причина: для видеоплеера она не оправдывает отдельный UI/control/runtime complexity; обычные repeat modes и explicit Stop достаточны.

## Removed vertical slice
- UI: удалены checkbox/tooltip из `ui/playlist/toolbar.rs`, `PlaylistAction::SetStopAfterCurrent`, post-render adapter branch и `PlaylistInteractionModel::{stop_after_current,stop_after_current_available}`. Toolbar regression test явно запрещает возврат текста/действия.
- App controller/runtime: удалены `StopAfterCurrentLatch`, controller field/accessors, `StopAfterCurrentOutcome`, `DeferredTransportIntent::StopAfterCurrent`, terminal executor branch, discovery runtime toggle adapter и все cleanup assignments, существовавшие только для latch lifecycle.
- EOF policy: clean Ended и detached tombstone больше не имеют product-specific stop-after-current branch; они сразу используют canonical `RepeatMode`/automatic traversal policy.
- Cross-crate cancellation API: удалён `MediaInstallCancellationCause::StopAfterCurrent` из `player-core` и `DiscoveryCancellationCause::StopAfterCurrent` из `playlist-discovery`; exhaustive mappings/tests обновлены. Остальные typed causes не свёрнуты в generic Cancelled.

## Preserved invariants
- `RepeatMode::{StopAtEnd, RepeatQueue, RepeatOne}` и main player repeat/shuffle controls не менялись.
- Manual Next/Previous, exact Play, explicit Neutral Stop, D08/D39 install barrier, D42/D50/D53-D57 holds, D26 deferred automatic continuation/cancel, tombstone continuation/Undo и suspend/resume lineage semantics сохранены.
- Независимый кусок старого combined test сохранён как `deferred_automatic_cancel_is_terminal`; tombstone/Undo и suspend-resume tests переименованы/очищены только от удалённого latch.
- Изменение является узким feature removal без косметического refactor: 26 files, 526 deletions, 11 additions at completion self-review.

## Verification
- Context7 egui docs confirmed immediate-mode widget removal requires no retained widget cleanup.
- PASS: `cargo test -p player-core --lib` (537), `cargo test -p playlist-discovery --lib` (52), `cargo test -p app-egui --no-default-features` (630), `cargo test --workspace --all-features --no-fail-fast`.
- PASS: `cargo +1.96.0 check --workspace --locked`, strict `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo fmt --all --check`, `scripts/check-refactor-guardrails.py`, `git diff --check`.
- Serena symbol overview reflects the removed variants. One rust-analyzer diagnostic for `transport.rs` remained stale and still named the already-deleted enum variant, while Cargo check/test/strict Clippy all compiled the exact current source successfully.
