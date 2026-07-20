# Playlist state

Session 06 completed PASS on 2026-07-14. This memory complements `mem:core`, `mem:playlist/core`, and the handoff in `user/playlist_queue_implementation_plan.md`.

## Ownership and dependencies
- New workspace crate `playlist-state` owns disk schema v1, JSON serialization/deserialization, bounded inspection, inspected-source identity, and explicit quarantine.
- Dependency direction is `playlist-state -> playlist-core` plus direct `media-core` access required by public neutral metadata value types. Normal implementation dependencies are `playlist-core`, `media-core`, `serde`, `serde_json`, `sha2`, and `libc`.
- `playlist-core` remains serde- and I/O-neutral with only `media-core` and `rand` normal dependencies.
- Caller supplies the exact state path/config directory. This crate does not read AppConfig/ConfigPaths, own the D10e instance lock, integrate app/UI/player/demux, or implement a save worker.

## Public boundary and schema
- Filename is `playlist-state.json`; current required top-level `schema_version` is integer 1.
- Serialization accepts immutable `PlaylistStateSnapshot { queue: &PlaylistQueue, repeat_mode }`; load yields `LoadedPlaylistState { queue, repeat_mode }`.
- Typed inspection outcomes are Missing, Loaded, CorruptNeedsQuarantine, NewerSchemaSaveBlocked, and UnrecognizedVersionSaveBlocked.
- Typed quarantine outcomes are Applied, SourceChanged, and FailedSaveBlocked. Inspection is read-only; quarantine happens only through the explicit app-policy call.
- Private v1 DTO uses explicit DTO/domain mapping and deny-unknown-fields. It persists exact canonical items, required nullable current, repeat, shuffle/history/cursor/upcoming, retained factual history tail, required `next_item_id`, full D12 display/sort cache and fingerprint.
- Domain restore remains the owner of queue invariants: capacity 50,000, unique non-zero IDs, required watermark strictly greater than max ID, references, cursor, repeated factual history, and duplicate-free exact upcoming. There is no max+1 allocator fallback and valid current None is not repaired to the first row.

## Locator and secret safety
- Raw direct URLs are read only through `SecretUrlLocator::expose_secret_for_persistence`. Public errors/outcomes and Debug snapshots do not format secret locators automatically.
- A normalized YouTube locator is persisted as the already-provided domain URL identity; there is no service dependency.
- Every local DTO carries exact origin platform (Linux, MacOs, Windows, Other) even for UTF-8 and exact encoding (UTF-8, bytes, wide, opaque units).
- Only matching native platform/encoding constructs PathBuf. Foreign or unsupported platform locators stay opaque and serialize again without lossy conversion.

## Bounded inspection
- A streaming envelope pass precedes supported DTO parse and scans the entire top-level object.
- Hard envelope limits are 40 MiB total, 1 MiB decoded token, and depth 128. Exactly one integer top-level schema_version is required; nested spoof is ignored.
- Duplicate/conflicting version keys, malformed/missing/non-integer version, or budget exhaustion before full uniqueness proof produce protected no-touch save-block.
- Newer schema is protected before applying current-v1 payload limits.
- Only after schema v1 is proven does the 32 MiB supported-v1 file limit and per-field/collection validation apply. An oversized proven v1 inside the envelope budget may be classified corrupt.

## Identity and quarantine
- Inspection uses no-follow regular-file classification. Identity contains platform file ID when available (Unix device+inode), length, mtime, and SHA-256 digest of inspected bytes.
- `apply_quarantine` reopens no-follow, revalidates metadata and digest, and then renames to `playlist-state.corrupt-<timestamp>.json` without collision overwrite.
- Linux uses `renameat2(RENAME_NOREPLACE)`; the fallback preserves no-overwrite with link/unlink semantics.
- There is no portable atomic compare-identity-and-rename. In-process writer/quarantine operations must share one `PlaylistStateStore` mutex, but an external TOCTOU window remains after revalidation and is explicitly documented.
- Deterministic naming is injected through `QuarantineFileName::from_timestamp`.

## Verification and next scope
- 15 playlist-state tests plus 60 playlist-core tests pass. Coverage includes full cache/URL/path variants, persisted allocator high watermark across remove/Clear/load, strict version envelope cases, all supported corrupt invariants, no-follow inspection, explicit quarantine collision/source-change/rename-failure, and newer-schema no-touch.
- Strict focused Clippy, fmt, Rust 1.96 and MSRV 1.92 locked workspace checks, guardrails, and git diff checks pass.
- Dependency audit still has only the known D28 blocking quick-xml advisories; the known audiopus_sys advisory remains non-blocking.
- Serena rust-analyzer diagnostics for `dto.rs` were stale after the module split (reporting removed symbols), so fresh Cargo checks/tests/Clippy are the authoritative verification for this session.
- Session 07 completed PASS on 2026-07-14. Atomic save, permissions, latest-only worker, D69 retry and D68 bounded shutdown are now implemented; app/config/UI wiring remains intentionally absent.

## Session 07 immutable snapshot and atomic writer
- Public `SaveRevision` is monotonic and checked; `ImmutableSaveSnapshot::capture` maps one borrowed `PlaylistStateSnapshot` into an opaque owned private v1 DTO. Items and matching `next_item_id` are captured together. Neither worker nor writer can rebuild, repair, or substitute the allocator watermark. JSON encoding runs on the writer thread.
- `AtomicSnapshotWriter` shares the existing `PlaylistStateStore` operation mutex with inspection/quarantine. It creates an exact owned unique `create_new` temp in the target directory, uses mode 0600 on Unix, serializes complete bounded pretty JSON, flushes and syncs the temp, drops the handle, performs same-directory rename, then syncs the parent directory.
- Public durability vocabulary is `AtomicWriteOutcome::{NotReplaced, ReplacedDurabilityUnconfirmed, Durable}` with typed stage/cause. A post-rename directory-sync failure is never treated as rollback: targeted retry calls only parent-directory sync and never rewrites an old revision.
- Temp cleanup is exact-path RAII for files created by that attempt only. There is no wildcard cleanup and foreign temp-looking files are never removed. If cleanup itself fails during Drop, the owned file remains user-only; startup does not blindly scan/delete it.
- Crash durability is limited by actual OS/filesystem/network-mount primitives. Successful rename with failed/unsupported directory sync remains `ReplacedDurabilityUnconfirmed`, not success.

## Session 07 worker lifecycle
- `SaveWorker::start` is fallible and returns typed `ThreadSpawn(io::ErrorKind)`; the OS spawn boundary is injectable in focused tests. Caller passes validated `SaveDebounce` (inclusive 250 ms..=30 s) and an app-owned `SaveWakePort`; the crate does not read config and has no winit dependency.
- Startup access is explicit `SaveWorkerAccess::{Writable, SaveBlocked(reason)}`. Block reasons cover newer schema, unrecognized version, duplicate version, quarantine failure and quarantine source change. Blocked start creates no thread and performs no target I/O; a new startup/reload decision is required instead of retry.
- Command capacity is 8. Accepted snapshots are strictly increasing; same/older revision is a typed no-op, and full/disconnected submission returns ownership of the snapshot. The worker drains/coalesces queued commands before I/O and runs at most one physical write.
- Terminal attempt mailbox capacity is 8 with one shutdown reserve. Attempt reports are lossless under the production scheduler, warning state coalesces, and disconnect has a separate exactly-once slot. Wake uses a false-to-true atomic edge plus drain-clear-recheck to close the publish-vs-clear race. Empty drains do not occupy command capacity.
- D69 scheduler uses an initial 1 s retry delay, doubles to a 60 s cap, resets/re-arms on a new mutation, supports manual immediate retry, and retains the latest dirty snapshot. Equal warning failures coalesce with a saturating occurrence count. Timers use blocking channel deadlines; there is no 50 ms polling or busy loop.
- `shutdown(newest_committed, timeout)` explicitly supplies the newest committed-only snapshot, bypasses debounce/backoff, and returns exact `ShutdownCompletion`. Timeouts distinguish command admission, filesystem acknowledgement and thread exit. Join is attempted only after `JoinHandle::is_finished`; a timeout never claims durability. Drop/channel disconnect does not perform a hidden flush. Future Session 14 integration must call explicit shutdown and, on timeout, follow terminal process-exit policy without releasing the D10e lease before the writable owner.
- Production layout after self-review: `worker/mod.rs` facade/runtime (711 lines), `worker/types.rs` (280), `worker/mailbox.rs` (163), `atomic_write.rs` (about 300), and `snapshot.rs` (about 80). No AppShell/config/UI wiring was added.

## Session 07 verification
- 33 playlist-state tests pass: the original 15 load/quarantine tests plus 18 worker/atomic tests covering revision/watermark pairing, debounce/latest coalescing, mutation during write, single-write invariant, full queue/backpressure, no-op revision, wake coalescing and publish-vs-clear, typed spawn failure, atomic target/no-partial behavior, owned-temp cleanup, Unix permissions, targeted directory retry, D69 cap/reset/manual/new mutation/reschedule, D68 committed-only flush/timeout, all save blocks, exactly-once terminal reports and wake disconnect.
- PASS: `cargo test -p playlist-state`; `cargo clippy -p playlist-state --all-targets -- -D warnings`; `cargo fmt --all --check`; `cargo +1.96.0 check --workspace --locked`; `git diff --check`; Serena diagnostics for new production modules.
- Sessions 08–13 are complete. App-side inspection/load gate, save-worker wake/debounce/view integration and D10e/D68 lifecycle ordering belong to Session 14; restored-current open remains later scope.


## Session 14 app integration (2026-07-15)

- `PlaylistRuntime` now owns read-only inspection, explicit identity-revalidated quarantine, protected-state writer blocking, exact dirty snapshots, save worker wake/retry/durability view and terminal flush. Valid watermark is retained even when restore apply is superseded; Missing/quarantine success create persistent lineage, protected outcomes create only non-persistent generation-scoped lineage.
- App integration and shutdown ownership details: `mem:app-egui/playlist-persistence-s14`.

## Current-media resume sidecar (2026-07-19)

- Frequent position persistence is deliberately separate from queue schema v1: `playlist-state::resume` owns strict `playlist-resume.json` schema v1, locator fingerprinting, quarantine, atomic latest-only writer and bounded terminal report; queue writes are never triggered by position updates.
- Runtime/startup/config ownership and verification: `mem:playlist/resume-position-sidecar-2026-07-19`.


## S01P read-boundary migration (2026-07-20)
- playlist-state validation now uses `retained_item_count()` and borrowed `iter_playable_items()`; DTO capture uses `OwnedPlayableItemsSnapshot` as the explicit persistence ownership handoff before private v1 DTO materialization.
- Disk schema v1, JSON ordering/fields, allocator watermark pairing, repeat/shuffle/current semantics, locator encoding and bounded validation behavior are unchanged. State tests no longer depend on queue slice/index/ambiguous len and retain full duplicate/non-UTF/allocator regression coverage.
- Verification: all 40 playlist-state tests PASS on Rust 1.96; strict focused Clippy, Rust 1.96 workspace check, focused MSRV 1.92 check, rustfmt, Serena diagnostics and guardrails PASS. No dependency change; cargo-deny remains blocked only by known quick-xml RUSTSEC-2026-0194/0195.


## S01C group-aware shuffle boundary before schema v2 (2026-07-20)
- Core `ShuffleTraversalSnapshot::upcoming()` теперь возвращает top-level `PlaylistEntryId`, а factual history по-прежнему exact `PlaylistItemId`. Private disk schema v1 не изменена: load мапит каждый legacy numeric upcoming ID в `PlaylistEntryId::Single`, поэтому все v1 fixtures/roundtrips сохраняют прежние bytes/semantics.
- Schema v1 структурно не умеет Group ID/parts. До S02 capture fail-closed возвращал `CompoundQueueRequiresSchemaV2`; этот временный limitation снят schema v2.
- S02 (2026-07-20) поднял current playlist-state writer schema до v2 при strict dual reader v1/v2. V1 DTO остаётся строгим migrator-ом: legacy items становятся top-level Singles, legacy upcoming numeric IDs — `PlaylistEntryId::Single`, Group allocator стартует с 1, durable import payload отсутствует. Newer schema protection теперь начинается с версии 3.
- Private v2 DTO сохраняет exact top-level Single/Compound order, Group ID + redundant membership/ordinal, оба allocator watermark, exact current part, factual Item-ID history и tagged Entry-ID upcoming. Atomic `ImmutableSaveSnapshot` materializes entries и оба watermark из одного immutable queue borrow; writer не вычисляет allocator значения самостоятельно.
- V2 durable payload хранит checked playback spans, bounded ancillary hints, provenance, availability и closed `DurableReopenLocator::{Local,Url,Service}`. Service DTO допускает только stable webpage/original/extractor material kinds; headers/cookies/Authorization/format/manifest/fragment/key/signed endpoint отсутствуют в DTO shape и rejected strict serde. Raw acknowledged URL/service identity присутствует только в JSON; Debug/errors остаются redacted.
- Native/foreign path mapping переиспользует exact v1 path DTO: matching native encoding восстанавливается в `PathBuf`, foreign/unsupported platform units остаются reversible и повторно сериализуются без lossy UTF-8.
- Resume sidecar `playlist-resume.json` остаётся отдельным schema v1; S02 не менял его DTO, writer или filename. Focused queue/resume isolation regression остаётся PASS.
- S02 verification: 47 playlist-state, 122 playlist-core и 722 app-egui tests PASS; strict Clippy для core/state, Rust 1.96 workspace locked check, focused MSRV 1.92 check, rustfmt, refactor guardrails и diff check PASS. Rust-analyzer показал stale unresolved-module diagnostic только на новом `dto/v2/payload.rs` parent declaration, тогда как сам child file diagnostics и authoritative Cargo/Clippy checks чистые.

## S04 neutral atomic-file-store extraction (2026-07-20)
- Новый std-only workspace crate `atomic-file-store` стал единственным владельцем filesystem durability protocol: exact target validation, bounded collision-safe same-directory `create_new` temp, Unix creation mode 0600, complete write/flush/temp `sync_all`, rename, parent-directory `sync_all` и exact-path RAII cleanup без scan/wildcard.
- Neutral public boundary принимает только exact target path и готовые bytes. Typed outcome сохраняет `NotReplaced` до rename, `ReplacedDurabilityUnconfirmed` после rename и `Durable`; targeted `sync_parent_directory` никогда не создаёт temp и не переписывает payload.
- `playlist-state` сохраняет serialization, queue/resume DTO, общий operation mutex с inspection/quarantine, worker retry/backoff и прежние публичные `AtomicWriteOutcome`/stage/cause types. Его adapter исчерпывающе переводит neutral outcomes, поэтому app/read-model API и поведение не изменились. Оба queue-state и resume writer используют один neutral protocol.
- Focused evidence: 8 `atomic-file-store` tests покрывают real replace/0600, invalid target, create/write/flush/temp-sync/rename failures, post-rename durability-unconfirmed, exact cleanup, collision continuation и 32-attempt exhaustion без wildcard; все 47 прежних `playlist-state` tests и 19 app persistence tests PASS.
- Guardrail фиксирует `atomic-file-store` как обязательный std-only neutral crate и разрешает `playlist-state -> atomic-file-store`, не разрешая обратную или app/player/UI/service dependency. Verification: Rust 1.96 focused tests, strict focused Clippy, Rust 1.96 workspace all-features locked check, focused MSRV 1.92 check, rustfmt, diff check, Serena diagnostics и refactor guardrails PASS.


## S10 export isolation note (2026-07-20)
- Pure M3U8/XSPF export не переиспользует private playlist-state DTO/serializer и не меняет schema v2, queue/resume atomic writers, save revision или dirty state. Downstream `playlist-io::PlaylistExportSnapshot` снимается отдельно через public immutable queue read boundary.
- S11 сможет передать готовые export bytes в `atomic-file-store`, но не через playlist-state worker/schema. Full contract: `mem:playlist/io-s10-export-2026-07-20`.
