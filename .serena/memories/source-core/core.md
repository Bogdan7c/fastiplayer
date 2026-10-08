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

## HTTP Range reconnect (UX16, 2026-10-08)

- Владелец политики — `src/http_reconnect.rs`: `HttpReconnectPolicy` (бюджет = `network.reconnect_wait_ms`, default 60 с, `0` = без повторов; `SourceRuntimeConfig::reconnect_policy()`, в `for_tests` выключено) и чистое `HttpReconnectSchedule::record_failure(now, retry_after)` (паузы 0,5→1→2→4→8 с, cap 8 с; `Retry-After` cap 60 с; пауза обрезается остатком бюджета; бюджет от первого сбоя одного логического read-а). `wait_before_reconnect` опрашивает cancellation каждые 10 мс.
- Цикл повторов — `src/http/range_reconnect.rs` (дочерний модуль `http`, видит приватные поля `HttpRangeSource`): `read_range_with_reconnect` сохраняет частично полученный body (`RangeReadFailure { received_bytes, error }`) и продолжает с первого недостающего байта; `ensure_same_representation` сверяет каждый `206` с probe (total length; ETag/Last-Modified только если есть у обеих сторон) → `HttpRepresentationChanged`. Счётчик `RangeDiagnostics::reconnects`.
- Классификация: `SourceError::is_transient_network_failure()` (timeout/request/body/short body, 5xx/408/429; НЕ 401/403/404/410 — подписи yt-dlp чинит VOD endpoint recovery) и `http_retry_after()`. Range-ответы 429/503 теперь несут `Retry-After`.
- `SourceError::SourceClosed` — «владелец закрыл источник». Намеренно не `Cancelled`: адаптеры мапят `Cancelled` в `io::ErrorKind::Interrupted`, а Symphonia/`read_exact` повторяют `Interrupted` бесконечно. Ставит `media-prefetch` по lifecycle token.
- `HttpSourceHop::Seekable(Box<HttpRangeSource>)` (clippy large_enum_variant).
- Тесты: `src/http_reconnect/tests.rs`, `src/http/tests/reconnect.rs` (loopback), `src/http/tests.rs::interrupted_range_response_resumes_from_first_missing_byte`. Сквозные — `media-source-open/src/direct_progressive_open/network_drop_tests.rs`. Полная картина — `mem:media-services/progressive-http-s22-2026-07-22` (UX16).

Related: `mem:media-services/manifest-supersede-cancellation-aud020-2026-08-24`, `mem:media-services/core`, `mem:testing/coverage`.
