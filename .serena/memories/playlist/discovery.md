# Playlist discovery

Session 09A completed PASS on 2026-07-14. This memory complements `mem:core`, `mem:playlist/core`, and the handoff in `user/playlist_queue_implementation_plan.md`.

## Ownership and dependencies
- `playlist-discovery` is the UI/player/config-neutral owner of single-local-file probe, bounded deterministic non-recursive directory manifests, immutable discovery requests/policy, one shared bounded executor, D43/D74 frontiers, and job-owned result delivery.
- Normal dependencies remain `media-core`, std-only `natural-sort-key`, `source-core`, `symphonia-demux`, and `thiserror`. It does not depend on `playlist-core`, app/UI/player/config/service crates, winit, async runtimes, or concrete backends.
- The crate never mutates `PlaylistQueue`, allocates Item IDs, opens playback, selects shuffle targets, or decides app commit/supersede. App wiring begins in Session 10A+.
- Session 09A modules are `policy`, `cancellation`, `request`, `executor`, `frontier`, `mailbox`, bounded `readiness_ack`, `stream`, `job`, and the small app-facing `handle`. After self-review the largest new production owner is `job.rs` at 792 lines.

## Probe and manifest foundation
- Public `probe_one_local_media(&Path, &CancellationToken)` returns immutable `ProbedLocalMedia` or typed `ProbeOneLocalMediaError`; topology admission includes null/unknown codec IDs and runs no packet/decode loop. Session 10C explicit Play D64/D75 uses a single prepared/open envelope instead of probe-first. The new pure `classify_local_media_tracks(&[TrackInfo])` reuses discovery-owned `LocalMediaKind` vocabulary for an already opened playback demuxer without I/O or queue policy.
- `build_directory_manifest` fixes D63 immediate-parent membership before scheduling. Records expose job-local key, original locator, natural position, and bounded alias diagnostics; canonical identity remains private dedup/validation state.
- D73 limits remain `RAW_MANIFEST_MAX_ENTRIES = 100_000` and `RAW_MANIFEST_MAX_PATH_KEY_BYTES = 64 MiB`; overflow returns no arbitrary prefix.

## Session 09A policy, requests, and executor
- `SiblingFilter::{VideoOnly, AllMedia, AudioOnly, SameAsOpened}` is topology-only. `SiblingDiscoveryPolicySnapshot` captures load/filter/revision once. Freeze, resume, cancel, ACK, admission flush, and event drain share one per-job linearization boundary.
- Request kinds are Sibling, ManualBatch, VisibleRefresh, and MetadataSortPreparation. The named request-item limit is 100,000 and Batch keys are constructed only after checked admission. `load_siblings=false` creates zero work and completes without probe I/O. VisibleRefresh expects caller-side D31 dedup by row identity/revision and deliberately preserves duplicate paths as distinct ordinals because this crate has no Item ID.
- Work units, not whole jobs, carry scheduling class: initial nearest-forward sibling and reprioritized manifest keys use the foreground lane; sibling tail/visible/sort work stays speculative. Neutral hints contain no repeat/shuffle types and promote already-queued keys as well as job-local pending keys.
- Executor budgets: `available_parallelism().clamp(2, 4)`, one foreground-only worker, input 256 with 16 foreground-reserved slots, max 16 active jobs/max 15 speculative jobs, per-job queued share 16. The production frontier intent method enforces a 256-offset lookahead independently per direction relative to its contiguous cursor; reprioritize cannot bypass it, and frontier advance reopens/reschedules the next window. Per-job execution permits leave a general worker available on hosts with at least three general workers.
- Dispatch rotates fairly by job within each lane. Cancel/shutdown removes queued units and releases accounting before they can probe; already-running blocking calls remain honest/cooperative. A panicking probe is contained and completes the job exactly once as `ExecutorDisconnected`.

## D43/D74 frontier and stream
- Before/After frontiers release only contiguous terminal prefixes. Base quotas are 24,999/25,000 with total automatic sibling limit 49,999 excluding target. Unused quota transfers only after terminal side exhaustion; failures/ineligible candidates consume no quota.
- Named bounds: directional lookahead 256, verified/job-owned records 512, batch 32 records, diagnostics 64 plus omitted count, event envelope 1,024.
- `AdmittedBatch` carries job/request/policy correlation, frontier revision, optional D43 side accounting, and typed apply semantics. Sibling chunks support progressive atomic commit; ManualBatch and MetadataSortPreparation chunks must accumulate until terminal success and then apply once atomically; VisibleRefresh chunks are metadata refreshes.
- `AdmissionAdvanced` is marker-only. `FrontierReady` is exact-nearest non-shuffle readiness after ACK of the matching batch and reuses the actual frontier revision (not a hardcoded value). ACK while frozen is non-consuming and typed `AdmissionFrozen`. ACK state is readiness-only and bounded by construction to at most two entries (Before/After); manual/sort/visible and later sibling batches retain no ACK entry and return the documented stale/not-required outcome.
- Cancel clears all stale batch/marker/readiness events still owned by the job. Only one `AdmittedBatch` owns each released record.

## Wake, completion, and verification
- One `WakeCoordinator` belongs to the whole executor, not to individual jobs. All mailboxes share one false→true edge with clear/scan/re-arm. Progress remains latest-only and every job owns a lossless terminal slot from construction.
- PASS: 52 `playlist-discovery` tests and 60 neighboring `playlist-core` tests. Focused tests are split into a small shared harness plus executor/lifecycle and job/frontier/stream modules. Added review coverage includes per-job fair rotation, queued cancel suppression, work-unit promotion, shared multi-job wake edge, panic/disconnect terminal ownership, request cap, duplicate-visible ordinal preservation, apply semantics, deterministic D43 completion/flush interleavings with identical capped set/side counts, manual cancel disposal of job-owned successful chunks, the enforced 256-offset near-hole window, 100k non-readiness ACK characterization, disabled-policy no-probe, frozen ACK, and exact readiness revision.
- PASS: strict focused Clippy with `-D warnings`, fmt, Rust 1.96 and MSRV 1.92 locked workspace checks, rustdoc `-D warnings`, toolchain policy, 18 guardrail tests plus production script, `git diff --check`, and Serena diagnostics.
- Session 15 app orchestration is complete: process-lifetime app code maps committed repeat/shuffle state to neutral hints, validates record-bearing batches, and commits ID-less drafts through playlist-core while this crate retains D43/frontier/readiness ownership and remains free of app/controller policy types. Full contract: `mem:app-egui/playlist-discovery-s15`.

## Session 16 visible-refresh and diagnostics extension
- `VisibleRefreshLocator` carries the local path plus the caller-observed optional fingerprint without importing Item IDs. `DiscoveryProbe::read_fingerprint` is a separate no-demux operation: an exact match completes that work unit without `probe_one`; mismatch or absent expected fingerprint proceeds to the normal discovery probe. Explicit D64 Play does not use this path and still performs exactly one prepared target open.
- `DiscoveryFailureCounts` is a lossless terminal summary for unsupported-container, no-audio/video-tracks, and probe-failed categories. Counts are updated before bounded diagnostic-detail retention, so omitted details do not make Manual Add completion accounting ambiguous.
- App-owned structural revision and Item ID/locator/fingerprint revalidation remain outside this neutral crate. Session 16 app contracts and verification are documented in `mem:app-egui/playlist-discovery-s15`.

## Session 16A metadata Sort consumer contract
- The neutral discovery crate was not given Sort/queue/app ownership. Existing `MetadataSortPreparation` remains a bounded local-only batch request with `AccumulateUntilTerminalAtomicApply`; it returns verified records, progress, exact failure counts and typed terminal outcomes. App orchestration decides which missing/untrusted local rows need it, correlates records back to Item IDs, and performs revalidation/commit.
- Natural Sort and fingerprint-backed cached metadata create no discovery request. URL rows never enter local probing or network opening. Individual probe failures may remain a typed partial warning/missing sort group; cancellation/executor failure hands already verified records to the app-owned D44 salvage policy.


## G2 coverage reliability correction (2026-09-01)

- `resume::session09a_tests::job_stream_tests::cancellation_releases_frozen_verified_buffer_without_record_event` больше не пытается заморозить job до доказанного старта worker-а. Fixture использует existing blocking probe gate (`02-block-video.media`), ждёт exact started event, затем freeze, release, processed и cancel.
- Это test-only root-cause fix `201ab746`: production executor/frontier/cancellation semantics не менялись. Focused 20-repeat и полный crate suite 52/52 прошли; финальный stable coverage квалифицирован по `mem:testing/coverage`.

## Cancellation vocabulary correction (2026-07-18)
- После полного удаления product-фичи stop-after-current публичный `DiscoveryCancellationCause` содержит шесть причин: `UserCancelled`, `Superseded`, `TransportStop`, `StructuralInvalidation`, `LifecycleSuspended`, `LifecycleShutdown`. Удалённый `StopAfterCurrent` не должен возвращаться как неиспользуемый generic placeholder.
- Executor/job cancellation semantics, first-writer-wins, bounded cleanup и app-neutral ownership не изменены; 52 focused tests и full workspace all-features suite прошли.


## S01B structural anchor integration (2026-07-20)
- Neutral `playlist-discovery` crate ownership/API не изменились: records по-прежнему не знают queue IDs и не мутируют canonical queue.
- Domain/app commit boundary теперь использует `playlist-core::StableInsertionAnchor` с explicit `PlaylistEntryId`, а не ambiguous Item ID. Top-level compound anchor разрешён; subordinate part anchor и stale entry anchor дают разные typed atomic failures без allocator burn.
- Current local sibling discovery records/target commits остаются standalone Singles, поэтому app mapping конструирует `PlaylistEntryId::Single` осознанно. Будущая compound-aware discovery policy обязана передавать owning `PlaylistEntryId::Compound`; insertion никогда не выбирает позицию внутри parts.

## S07 nested local playlist ownership (2026-07-20)
- Recursive `.m3u`/`.m3u8`/`.xspf` import НЕ добавлен в `playlist-discovery`: общий local playlist filesystem/DFS/budget/cycle owner находится в `playlist-io::local_expansion`. `playlist-discovery` сохраняет single-media probe/directory-manifest/executor ownership и не получает dependency на `playlist-io` или queue authority.
- Canonical identity S07 transient и используется только active-stack cycle detection; reversible path/tree/budget/cancellation contract: `mem:playlist/io-s07-nested-local-expansion-2026-07-20`.
