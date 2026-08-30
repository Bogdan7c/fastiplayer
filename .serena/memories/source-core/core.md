# source-core — transport/runtime ownership

## Abortable HTTP task executor (2026-08-30)

- `source-core::AbortableHttpTaskExecutor<T>` owns physical async HTTP future replacement behind a Tokio-free public boundary. Adaptive providers own semantic generation/job-id/publication policy; source-core owns exactly-once delivery or physical cancellation of the current future.
- Command identity is a single `VersionedTaskSlot { revision, task: Option<Task> }`. A publisher serializes slot mutation and watch publication under the slot mutex: advance the application revision, store `Some(task)` or cancellation `None`, then send exactly that revision. Watch and slot must never have independent counters or uncorrelated identity.
- The worker copies the observed watch revision and drops `watch::Ref` before locking the slot. Publisher order is `slot mutex -> watch send`; acquiring `watch read guard -> slot mutex` is forbidden because it can invert the lock order.
- A worker may take a task only when `slot.revision == observed_revision`. On mismatch it leaves the newer task untouched and returns to `changed()`; the unseen newer watch revision is already pending, so this neither spins nor loses a wake. Cancellation `None`, revision wrap, biased abort of an in-flight future, result ownership and shutdown follow the same revision contract.
- Regression oracles:
  - `crates/source-core/src/abortable_http_task.rs::stale_observed_revision_leaves_newer_task_for_exact_notification`;
  - `crates/source-core/src/abortable_http_task.rs::immediate_successor_after_cancellation_completes_every_time`;
  - vertical current-generation proof: `crates/web-media-adaptive/src/tests/live_manifest_refresh.rs::live_manifest_refresh_fences_slow_stale_generation`.
- The vertical test uses a held first TCP request as a real rendezvous, not a timing sleep. It supersedes A with B, rejects stale publication, requires current generation/body and exactly two requests.

Related: `mem:media-services/manifest-supersede-cancellation-aud020-2026-08-24`, `mem:media-services/core`, `mem:testing/coverage`.
