# AUD-020 — abortable superseded manifest generation (2026-08-24)

## Дополнение 2026-08-30: revision-correlated task slot

- Финальная проверка repeatability выявила реальный lost-task race в `source-core::AbortableHttpTaskExecutor`: отдельные watch-revision и неверсированный latest-task slot позволяли worker-у, обрабатывающему старую cancel-ревизию, забрать уже новую task, после чего biased `changed()` дропал её и executor засыпал без result или компенсирующего wake.
- Slot теперь хранит единый `VersionedTaskSlot { revision, task }`. Publisher под одним mutex вычисляет следующую прикладную revision, записывает `Some(task)` либо cancel-`None`, затем публикует ту же revision в watch. Worker извлекает task только при точном совпадении observed и slot revision; mismatch сохраняет более новую task до следующего уже ожидающего notification.
- Lock order обязателен: worker копирует watch revision и освобождает `watch::Ref` до захвата slot mutex; publisher использует `slot mutex -> watch send`. Это запрещает инверсию `watch read guard -> slot mutex`. Empty cancellation и wrap revision используют тот же versioned contract.
- Алгоритмический oracle расположен в `crates/source-core/src/abortable_http_task.rs`: `stale_observed_revision_leaves_newer_task_for_exact_notification` и `immediate_successor_after_cancellation_completes_every_time`. Вертикальный held-request TCP rendezvous находится в `crates/web-media-adaptive/src/tests/live_manifest_refresh.rs::live_manifest_refresh_fences_slow_stale_generation`; он больше не использует scheduler-delay `sleep(40ms)`, требует ровно две HTTP-попытки и публикует только current generation/body.
- До исправления вертикальный тест воспроизводил timeout уже на 2-м из 100 отдельных запусков без workspace load. После исправления он прошёл 100/100; source-core successor oracle — 100/100 по 32 внутренних цикла. Публичный API и dependency graph не менялись.

Ниже сохранён исходный AUD-020 boundary; это дополнение уточняет внутреннюю реализацию latest-task slot.

## Independent verification

- Pre-fix hermetic loopback reproduction held generation A response body for 300 ms after headers, then superseded with B while continuously polling.
- Measurement: B first reached the server after 303 ms; `b_before_release=false`; TCP A was not disconnected before release. After A completed, only generation B/body was published, so stale publication fencing was already correct.
- Workspace reachability search found `AdaptiveManifestFetcher` only in its implementation, public re-export and tests. Current production HLS/DASH/Smooth/HDS manifest paths call `AdaptiveHttpContext::fetch_resource_blocking`; AUD-020 was a dormant public API defect, P3.
- Root cause: one blocking `FetchExecutor` worker serialized `recv -> reqwest::blocking send/read -> outcome`. Logical generation replacement owned no physical request lifetime; shared source cancellation could not cancel only A without also cancelling B.

## Final ownership and boundaries

- `source-core::HttpSourceSession` remains the single HTTP policy owner. Existing blocking client behavior is unchanged. A lazy async reqwest client is materialized only on the first abortable request, shares the same scoped cookie jar/config policy and is reused by all session clones.
- `HttpSourceSession::fetch_bounded_single_hop_abortable` is the abortable one-hop boundary. Blocking and async paths share request preparation plus redirect/status/Range/body accounting validation; body I/O differs, error/status/range/secret semantics do not.
- `source-core::AbortableHttpTaskExecutor<T>` hides Tokio completely. It owns one current-thread runtime thread, one latest task slot, one result slot and polls at most one boxed standard `Future`. A biased command branch drops the old future before polling its replacement.
- `web-media-adaptive` has no direct Tokio dependency and remains inside its guardrail allowlist. `AdaptiveManifestFetcher` owns generation/job-id/retry/publication policy, while the source executor owns physical future replacement.
- Supersede sends cancel/replace before updating/submitting the new manifest job. Dropping the reqwest future closes the superseded request/response. Source-wide cancellation and fetcher Drop use the same physical abort mechanism without cancelling future generations or blocking the caller on join.
- Same/older generation rejection before network, redirect/secret monotonicity, retry accounting, typed poll states and stale outcome rejection remain unchanged.
- Existing blocking segment/resource executor is unchanged.

## Tests and verification

- `source-core` focused tests cover executor replacement/cancel future Drop, successful full body, exact Range metadata, body bound, pre-cancel no-network and session reuse with a fresh cancellation lifetime.
- `web-media-adaptive::tests::manifest_cancellation` uses hanging loopback responses and requires physical TCP disconnect plus B start within a 750 ms test deadline, far below production timeout. Rapid A -> B -> C closes A/B and publishes only C; source cancellation and fetcher Drop also disconnect current work.
- Final affected suites: `cargo +1.96.0 test -p source-core -p web-media-adaptive --locked` => source-core 66/66, adaptive 44/44.
- Strict affected Clippy `-D warnings`, Rust 1.96 workspace check, Rust 1.92 MSRV workspace check, `cargo fmt --all --check` and refactor guardrails passed.
- Audit record: `user/project_health_audit_2026-08-22.md`.
- Context7 clarification: cancellation is based on dropping an async reqwest future. Tokio `spawn_blocking` is deliberately not used because already-running blocking work cannot be aborted.

Related: `mem:core`, `mem:media-services/adaptive-transport-s31-2026-07-23`, `mem:media-services/core`, `mem:testing/sandbox_policy`.