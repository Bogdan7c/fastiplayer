# PlaylistController Session 12A

Session 12A completed PASS on 2026-07-15. This memory extends `mem:app-egui/playlist-controller-s12` and `mem:playlist/core`.

## Ownership and boundaries
- `playlist-core::queue::removal` owns the opaque `PlaylistRemovalSnapshot`, typed `RemovalCurrentOutcome`, exact canonical/current/shuffle/allocator restore-as-new-mutation, and stale revision guards. Removing committed current always persists `None`; no successor is assigned before exact Installed.
- `PlaylistItem` keeps locator + cached metadata in a private `Arc` payload. Capturing/restoring a 50k removal snapshot clones row handles but never deep-clones locator/metadata strings; shuffle state is already Arc-backed. `Arc::ptr_eq` focused coverage proves payload sharing.
- `PlaylistController` owns destructive Remove/Clear/RemoveOthers, D47 selection fallback, D57 structural invalidation, detached `ActiveMediaIdentity { item_id: None, .. }`, tombstone continuation, revalidation, reattach and release.
- `PlaylistRuntime` owns exactly one process-lifetime `RemovalUndoState`: typed kind, controller/domain snapshot, pre-removal selection, active lineage correlation and an 8-second `Instant` deadline. UI/store/discovery do not own or serialize this slot.

## Tombstone and continuation invariants (updated 2026-07-19)
- Ordinary removal of an active playlist row preserves playback: it detaches the exact lineage/player instance/binding, creates the tombstone, and never sends player Stop or reopen.
- Clear is intentionally different. `controller/removal/clear.rs` performs the existing single atomic queue mutation, clears active identity and any old detached tombstone, creates no continuation/successor/reopen, and returns an exact `ResetMedia` request for any current media, including external or already detached media. Clear without active media creates no request.
- `PlaylistRuntime` owns one latest-only not-yet-enqueued Clear reset. The common frame transport driver uses nonblocking enqueue: `Full` retains it for retry; `Disconnected` terminates it with the safe message «Очередь очищена, но воспроизведение не удалось сбросить». A request-owned receipt is the only authority for app-side source/frame/timeline cleanup and `Stopped` commit.
- Receipt correlation is race-safe: matching `Applied` clears app media; stale with no current media is equivalent to already cleared; stale with a newer current instance or a controller with newly installed active media is superseded and cannot stop/clear the new media. Terminal reset failure never rolls the queue back.
- Remove active captures an opaque fixed traversal context before mutation. Ended revalidates its target/members against the current committed queue. RepeatOne is normalized to StopAtEnd for tombstones, so the deleted row is never replayed.
- Failed or cancel-winner navigation, repeated Ended, and same-lineage D72 rebind preserve an ordinary-removal tombstone. Only successful Installed of another lineage or process shutdown releases it.
- Successful continuation sets traversal current only through the existing exact Installed domain token. D57 matching already-Ended invalidation remains a typed Stop and contributes no second dirty revision.
- Same-lineage rebind requires the exact previously observed `ActiveMediaIdentity`; stale old instance/binding returns typed `Stale` and cannot overwrite a newer rebind.

## Undo invariants (updated 2026-07-19)
- Deadline expiry is exact at `now >= deadline`; countdown uses monotonic time and exposes only the next visible-label/expiry wake.
- A second removal replaces the slot with the state immediately before that second mutation. No-op removal preserves the previous slot.
- Any later real persistent playlist mutation or successful new-lineage install invalidates Undo. Selection and other non-playlist actions, failed candidates, and same-lineage/new-instance rebind preserve it.
- Matching-lineage Undo for ordinary removal restores queue/traversal/current/selection as a newer structural/dirty mutation and reattaches the current player instance without reopen. It never restores cancelled I/O or runtime error history.
- Clear Undo restores only rows/order/traversal current/selection. It never restores active media, playback, tombstone, or old resume position and never cancels a pending exact reset, including Undo before the reset receipt.
- Pre-Ready installs are retired and cooperatively cancelled after a successful removal transaction. Ready/reserved/dispatch/in-flight phases return typed `InstallCommitLinearizing` and keep D08/D39 ownership intact.
- The obsolete Session 11 `remove_non_active` bypass was removed; all future destructive callers must enter through the process-runtime removal boundary.

## Verification and scope
- PASS: 69 playlist-core, 33 playlist-state, 387 app-egui no-default, 23 player EOF drain, 6 exact transport; strict touched-crate Clippy, fmt, Rust 1.96 locked workspace check, refactor guardrails, diff check, and clean Serena diagnostics.
- Focused coverage includes all removal kinds, D47, all repeat modes, fixed continuation revalidation, failed/cancelled retention, exact Installed release, deadline boundaries, second removal, metadata invalidation/no-op, 50k sharing, matching/stale lineage, same-lineage new instance, shutdown and new-lineage release.
- Sessions 13/14 persistence ownership and Session 14B D72 suspend/resume are complete. Same-lineage resume uses the existing exact rebind boundary, preserves tombstone/Undo/stop latch, and carries an already consumed EOF edge to the rebound instance; genuinely new strong installs invalidate old-lineage runtime state through existing rules. Full checkpoint contract: `mem:app-egui/suspend-resume-checkpoint-s14b`. UI/discovery remain later scope.
## Undo удаления и metadata-патчи (UX A9, 2026-10-10)
- Undo slot сверяет не `dirty_revision`, а `PlaylistController::undo_blocking_revision()`: он растёт на любом `commit_dirty`, КРОМЕ чистых metadata-патчей (`commit_metadata_patches` использует `commit_metadata_only_dirty`). Фоновая длительность/теги/обложка не убивают Undo.
- `PlaylistQueue::restore_removal_snapshot` больше не требует равенства `metadata_revision`; проверяет allocator-ы, structural +1 и совпадение id-последовательности оставшихся строк; оставшиеся строки берутся из ТЕКУЩЕЙ очереди (свежие metadata), возвращаемые — из snapshot.
## Undo после смены текущего (UX A9b, 2026-10-10)
- Модуль `controller/removal/current_pointer.rs`. `PlaylistController::undo_survives_current_change(&removal)` = shuffle выключен И `removal.removed_only_non_playing_rows()` (не Clear, нет tombstone, `current_outcome == Preserved`).
- Если true: смена lineage (автопереход/ручной выбор) НЕ сбрасывает Undo (`availability(.., survives_current_change)` и проверка в `on_playlist_installed_with_playback_intent`), а restore идёт через `PlaylistQueue::restore_removed_rows_keeping_current`: возвращает только строки, traversal_current остаётся из текущей очереди (хранится по item id, поэтому сдвиг индексов безопасен), traversal revision не проверяется на «+1», shuffle в очереди/snapshot -> `RemovalSnapshotRestoreError::ShuffleActive`.
- Чтобы Installed не блокировал Undo, `install.rs` коммитит dirty через `commit_install_dirty`: без structural-изменения и без shuffle `undo_blocking_revision` не двигается (persistence-сигнал обычный). Любое structural/mode-изменение по-прежнему блокирует.
- С shuffle, при удалённой играющей строке и для Clear действует прежняя семантика (смена lineage сбрасывает Undo, restore откатывает pointer и shuffle-историю из snapshot).
- Тесты: `removal_undo/tests/current_change.rs`, `playlist-core/src/queue/removal/tests.rs`.
## Undo и боевой путь Installed (UX A9c, 2026-10-10)
- Корень регрессии A9b: боевой автопереход регистрируется не прямым `on_playlist_installed`, а через `PlaylistRuntime::register_successful_strong_install` (`suspend_resume.rs`, зовётся из `state::strong_media_open`), где было СВОЁ безусловное «lineage сменилась -> removal_undo = None».
- Единственное правило «новая lineage освобождает Undo» теперь в `removal_undo.rs::release_removal_undo_for_new_lineage` (учитывает `undo_survives_current_change`, пишет debug-лог target `rustiplayer::removal_undo`). ВСЕ пути регистрации установленного media обязаны звать его, а не сравнивать lineage сами.
- Тесты боевого пути: `removal_undo/tests/current_change.rs` (`InstalledPath::StrongInstall`).
