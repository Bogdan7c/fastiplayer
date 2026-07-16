# PlaylistController Session 11A

Session 11A completed PASS on 2026-07-14. This memory complements `mem:core`, `mem:playlist/core`, and `mem:app-egui/media-open-coordinator-s10c`.

## Ownership and module boundary
- Process-lifetime `PlaylistRuntime` owns one modular `PlaylistController`; renderer/window recreation never moves canonical controller state into `AppState`.
- Controller implementation lives under `crates/app-egui/src/playlist_runtime/controller.rs`, with D08/D39 protocol in `controller/install.rs`, identities/errors/latch in `identity.rs`, immutable view in `view.rs`, and focused tests in `controller/tests.rs`.
- `playlist-core::PlaylistQueue` remains the only owner of canonical items, monotonic Item ID allocator, traversal current, domain revisions, shuffle state, and opaque reservation token.
- `MediaOpenCoordinator` remains policy-neutral. Controller exposes opaque Start/Coalesce/Supersede decisions without queue target or priority; coordinator still owns preparation/staging/authorize/cancel/terminal mechanism.
- Renderer-bound `AppState` receives a validated `PlaylistAppStateAttachment`: exact `PlaylistRuntimeBinding` plus immutable `Arc<PlaylistViewSnapshot>`. Attachment logic is isolated in `state/playlist_attachment.rs`; mutable controller access stays in `PlaylistRuntime`.

## Identities and view
- Traversal current, selected row, `PendingTarget`, `ActiveMediaLineageId`, exact `MediaInstanceId`, and player binding generation are independent identities.
- `ActiveMediaIdentity` stores optional Item ID + app lineage + exact player instance + binding generation. A normal successful install allocates a new app lineage; stop-after-current is only a storage shell in 11A.
- Runtime item errors are bounded one-per-Item-ID records with exact request/instance correlation. Retry does not clear the badge; exact same-item Installed clears it. D70 unavailable committed sources remain rows and do not create dirty state.
- `PlaylistViewSnapshot` stores structural rows as shared `Arc` data. Rows/labels rebuild only after structural mutation; selection/error/pending/active updates reuse row storage. `visible_rows(range)` performs work proportional to the requested visible range.
- Structural/view/dirty revisions are separate. Selection, runtime errors, pending state, reservation prepare/abort, typed no-op, and protected runtime generation do not dirty persistent state.

## D08/D39 guard
- Guard phases are `ReservedAwaitingAuthorization`, `AuthorizationDispatchPending`, and `AuthorizationInFlight`.
- Matching Ready runs fallible `PlaylistQueue::prepare_reserved_mutation` before authorization request. Reservation failure preserves old queue/active state and burns no ID/high-watermark.
- Coordinator command acceptance only enters dispatch-pending. The exact token remains held until lossless `AuthorizationDispatchResolution`; delay never authorizes timeout abort.
- Cancel-win and downstream pre-enqueue rejection exact-abort the token. `EnqueuedAtPlayerOwner` enters in-flight and requires exact Installed. Missing/mismatched resolution, request, player request, or Installed terminal becomes sticky fatal invariant state.
- Terminal drain order is fixed: domain commit/abort, then one coalesced `DesiredQueueModes`, then one deferred Stop/Suspend/Shutdown intent. No command FIFO exists.
- Structural append/remove is blocked by the domain reservation. Append is no-play. Non-active removal applies D47 selection fallback and removing persisted current leaves current None. Active removal is typed blocked until the later tombstone session.

## Verification and limitations
- Focused controller tests cover reservation success/failures, acceptance vs barrier, delayed resolution, cancel/rejection/enqueue, exact Installed, missing/mismatch fatal paths, mode/lifecycle drain, append/remove, D47/D49/D61/D70, shared visible access, worker unavailable, and stop-latch storage.
- Session verification: 61 playlist-core tests and 325 app-egui no-default tests, strict focused Clippy, fmt, Rust 1.96 locked workspace check, refactor guardrails, and diff check PASS.
- Serena may report stale unresolved-module diagnostics for newly created untracked module files while Cargo test/check/Clippy resolve them correctly.
- Explicitly not implemented in 11A: Next/Previous, fast traversal preview, Ended execution, active tombstone transition, discovery orchestration, persistence/JSON/settings/store, and playlist UI. Next allowed work is Session 11B only.
## Session 16A Sort controller boundary
- `controller/sorting.rs` owns the app preflight/commit adapter for prepared canonical Sort. It verifies fatal invariant state and exact app structural revision, delegates domain permutation/metadata preflight, preflights app dirty/structural counters, then exposes an infallible commit used only inside one runtime terminal drain. `controller/metadata.rs` now provides the same owner-correct two-phase boundary for metadata-only patches: fatal/domain revision/app dirty failures occur before it returns `PreparedControllerMetadataPatchCommit`; the following commit is infallible.
- Reorder invalidates manual-navigation transient state and rebuilds rows once, but selected row, active media, pending target, traversal current, runtime errors and Item IDs are not rebound. Metadata-only Sort/salvage does not create a structural revision or traversal mutation. Persistent Sort changes invalidate removal Undo before apply and publish one dirty/save revision; no-op preserves Undo and dirty revision.
