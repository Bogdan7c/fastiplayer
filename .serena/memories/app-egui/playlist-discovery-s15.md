# App target-first sibling discovery (Session 15)

Session 15 completed PASS on 2026-07-15. This memory complements `mem:core`, `mem:playlist/core`, `mem:playlist/discovery`, `mem:app-egui/media-open-coordinator-s10c`, and the handoff in `user/playlist_queue_implementation_plan.md`.

## Target-first ownership and exact install chain
- Explicit local picker and trusted CLI admission both reach the same asynchronous `LocalFileOpenJob`; UI-thread local preparation was removed.
- The job calls Session 10C `prepare_local_open` exactly once and transfers the full `PreparedLocalOpenResult`: already opened `PreparedMedia`, topology classification, duration/full metadata, original native source path, safe label, and fingerprint.
- App builds the target queue draft from the original source path; canonical manifest identity never replaces the locator or source label.
- Exact player staging request is correlated with a controller D08 target-only replacement intent. Only player `ReadyToCommit` triggers fallible queue reservation; then controller begins authorization dispatch, the coordinator returns the authoritative barrier resolution, and exact `Installed` infallibly consumes the token.
- Admission/reservation/revision/allocator/capacity/downstream cancellation failure keeps the old queue, active playback, and allocator watermark and cannot start sibling scan. Sibling discovery starts only after exact target commit exposes the reserved stable target Item ID.
- In-app nonempty replacement still requires the D79 confirmed origin. CLI uses the separate typed trusted startup admission.

## Process-lifetime discovery coordinator
- `playlist_runtime::discovery::PlaylistDiscoveryCoordinator` belongs to `PlaylistRuntime`, not renderer-bound `AppState`; playback advance within the committed queue does not end the scope.
- One controller-owned `SiblingDiscoveryScopeId` type is shared by coordinator, settings transaction port, and future readiness waits; it is distinct from `MediaInstanceId` and `PlaylistItemId`. Active authority also captures discovery job/request/policy revisions and a controller-owned `DiscoveryContinuation`.
- `discovery/manifest_worker.rs` owns one reusable bounded blocking filesystem worker and join authority. `discovery/mapping.rs` owns conversion between app policy/domain records and neutral discovery hints/drafts. `discovery/settings_port.rs` owns the D62 phase intent and exact shared freeze/finalize control. The central coordinator stays below the project module-size threshold.
- `load_siblings=false` completes target-only without manifest/probe I/O. Manifest overflow/failure, executor rejection, cancellation, or later discovery failure preserves target and already accepted batches and publishes a typed target-only/final status.
- Repeat/shuffle policy is converted in app code to ordered neutral `ReprioritizeHint` keys; `playlist-discovery` remains free of playlist policy types.
- D62 production port freezes exact pending-manifest admission or the exact active job, rollback resumes it, and post-persist finalize cancels it. Enabling sibling loading is future-only and changing filter/revision affects only future scopes.
- A record-bearing batch published immediately before freeze may commit while its readiness ACK returns `AdmissionFrozen`; the coordinator retains at most two directional ACK IDs and retries them after exact resume. Failed freeze/resume is a typed settings-stage error rather than an ignored boolean.
- Shutdown closes manifest admission and cancels discovery before persistence flush without waiting for blocking filesystem work; Drop retains join authority.

## Progressive commit invariant
- Only record-bearing `AdmittedBatch` can mutate the queue. `AdmissionAdvanced` and `FrontierReady` are marker-only and never carry or commit records.
- Every batch validates job/request/policy revision, direction, apply semantics, and controller continuation. Natural anchor calculation uses committed manifest-key→stable-ID registry; drafts are ID-less until the domain mutation.
- `playlist-core::PlaylistQueue::insert_discovery_batch` atomically preflights D08 lock, exact structural queue revision, stable anchor membership, 50k capacity, next structural revision, and allocator range. Only then does it allocate stable IDs, splice once, and perform one D14b responsive-shuffle merge preserving the existing upcoming order.
- Controller publishes one structural/dirty revision per accepted batch, advances its continuation to the exact new queue/controller revision, and therefore does not self-cancel. Coordinator binds manifest records only to stable IDs returned by the successful outcome and ACKs only after commit.
- Structural mutations cancel the scope; non-structural actions do not. D43/frontier ownership stays in `playlist-discovery`, so unresolved nearer holes and per-side quota/cap rules are enforced before app sees an admitted batch.
- UI-facing `PlaylistDiscoveryInsertionHint` contains inserted stable IDs and an optional stable before-anchor for future scroll compensation. Session 15 adds no playlist rendering.

## Wake and verification
- Manifest worker and discovery executor use the existing typed AppWake edge. App shell drains on wake and requests redraw only for a visible status/progress/batch change; no mandatory per-job 50-ms polling was added.
- PASS: 71 playlist-core tests, 33 playlist-state tests, 470 app-egui no-default tests, 52 playlist-discovery tests, focused local-open/controller discovery tests, strict touched-crate all-target Clippy, fmt, Rust 1.96 locked workspace check, refactor guardrails, and diff check. Cargo is authoritative; rust-analyzer retained stale diagnostics for the updated local-open enum shape in `state/media_jobs.rs` and intermittently for the new test submodule, while all production discovery modules are otherwise clean.
- Manual Add, visible refresh, metadata Sort preparation, playlist UI rendering, and readiness-driven delayed navigation are outside Session 15. Next allowed session is 15A.
