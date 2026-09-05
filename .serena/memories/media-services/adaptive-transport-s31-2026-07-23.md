# S31 — Adaptive transport foundation (2026-07-23)

## S32B additive resource-fetch boundary

- `AdaptiveHttpContext::fetch_resource_blocking` is the provider-neutral blocking-worker API for bounded manifest/media/init/key resources over the same S31 session/retry/redirect/cookie/cancel policy. Query application is typed replacement vs HLS merge vs bypass; HLS/parser/AES/demux policy remains outside this crate. Consumer contract: `mem:media-services/hls-vod-s32b-2026-07-23`.

## Итог

- Новый workspace crate `web-media-adaptive` владеет shared provider-neutral manifest/segment lifecycle для будущих HLS/DASH/ISM/HDS consumers.
- Concrete manifest/container parsing, yt-dlp DTO, UI, queue, player live timeline, ABR и encryption policy намеренно отсутствуют.
- Existing finite `demux_api::OrderedSegmentSource` не менялся: S28G Symphonia/MPEG-TS/FLV consumers сохраняют прежний contract.

## Ownership и boundaries

- `source-core::HttpSourceSession` — единственный low-level HTTP policy owner. Existing `fetch_bounded_single_hop` сохраняет blocking reqwest path; `fetch_bounded_single_hop_abortable` добавляет lazy async frontend для физически отменяемого request future. Оба path-а используют общие request/redirect/status/Range/body accounting validators: full body ограничен caller bound, exact Range требует `206` + matching `Content-Range`, `200` остаётся typed unsupported, остальные statuses сохраняются как `HttpStatus`.
- `web-media-adaptive::AdaptiveHttpContext` создаётся из S21T `TransportOpenRequest` и переиспользует exact `HttpRequestTarget`, `SourceGeneration`, expected `MediaPresentation`, `RedirectPolicy`, scoped `SecretRequestContext`, cookie jar и shared cancellation. Второго HTTP policy/cache/prefetch owner-а нет; `HttpSourceSession` лениво создаёт отдельный async reqwest pool только для abortable future path и разделяет ту же scoped cookie/config policy.
- Manifest owner: `AdaptiveManifestFetcher` + `ManifestFetchRequest/ManifestPoll/ManifestResource/ManifestBaseUri`. Base URI всегда effective post-redirect target; relative references разрешаются через source-core WHATWG `HttpRequestTarget::resolve_reference`. Same/older claimed generation отклоняется до network side effect; source-owned `AbortableHttpTaskExecutor` одновременно poll-ит один boxed future и drop-ает superseded request до запуска current generation; stale outcome не публикуется. `web-media-adaptive` не зависит от Tokio.
- Segment owner: `AdaptiveOrderedSegmentSource` принимает bounded `AdaptiveSegmentSnapshot` и poll-ит `SegmentPoll::{Segment,TemporarilyUnavailable,EndOfStream,Failed,Cancelled}`. Readiness не кодируется как `None`, error, empty segment или fake EOF. Snapshot presentation обязан совпасть с S21T request. Same/older refresh rejected; overlapping newer live snapshot фильтрует уже delivered sequences, поэтому segment не отдаётся повторно.
- Full-resource segments ограничены `maximum_segment_bytes`; `SegmentByteRange` проверяет offset overflow и exact length. Retry policy typed/bounded: non-zero attempts, exponential capped delay, transient transport/body/timeout/status failures; cancellation и policy/secret/shape failures не retry-ятся.
- `AdaptivePresentation` даёт neutral VOD/live-edge/optional DVR vocabulary; `ComponentClockMetadata` хранится отдельно на каждый audio/video component и не смешивает timescale/origin.

## Player-owner / demux lifecycle

- `BlockingOrderedSegmentAdapter` — узкий bridge только для выделенного demux worker-а. Он может ждать poll readiness, но player-owner никогда не блокируется.
- `demux_api::ProgressiveDemuxer::new_deferred` переносит initial segment readiness, registry sniff/open и дальнейший parser loop на worker. До open player-facing `next_event` возвращает existing S21R `TemporarilyUnavailable`; после open первым публикуется `TracksChanged` и optional metadata.
- Deferred wrapper typed-отклоняет seekable inner вместо молчаливой потери seek contract. S31 не реализует VOD/DVR seek; это остаётся последующим transport/demux/player scope.
- Segment/resource blocking workers по-прежнему не join-ятся на player-owner и освобождаются на configured timeout/read boundary. Отдельный manifest future path физически abortable: supersede/source cancellation/Drop владельца уничтожают current reqwest future без blocking join.

## Selected-only bounded segment read-ahead — 2026-08-14

- `AdaptiveOrderedSegmentSource::new` сохраняет прежний single-fetch contract. Новый `new_with_read_ahead_concurrency` создаёт caller-bounded HTTP executor, но active fetch limit остаётся 1 до явного `enable_concurrent_read_ahead`; значит provider probe не получает скрытого prefetch. После enable несколько jobs могут завершаться не по порядку, однако `active + completed BTreeMap + pending` публикуют только минимальную outstanding sequence.
- `BlockingOrderedSegmentAdapter::new_activatable` возвращает adapter и intent-only `BlockingOrderedSegmentReadAheadHandle`. До `activate` adapter demand-only; после activation отдельный cooperative pump заполняет только caller-owned `NonZeroUsize` FIFO. Handle поддерживает idempotent activate, suspend/resume и cancellation-aware wait for first ready successor. Default `new` и все consumers, которые не выбирают activatable path, не меняют поведение.
- Success, terminal error, cancellation и EOF сохраняются distinct. Только успешно выданный segment обновляет `last_delivered_sequence`; failed fetch не является delivery receipt, поэтому новая manifest generation может повторить тот же sequence. Tests покрывают selected-only activation, bounded FIFO, suspend/resume, cancellation without network, successor failure after delivered content и recovery той же failed sequence в новой generation. HDS hermetic integration дополнительно доказывает concurrency high-water 2 и post-seek packet; HLS/DASH/Smooth suites остаются регрессией default path-а.

## Secret/redirect invariants

- Automatic redirects выключены. Каждый hop проходит S21T `RedirectPolicy`; cross-origin forwarding монотонно лишается header/query secret material.
- S36P3 additive boundary: `AdaptiveResourceSecretForwarding::{ForwardScoped, Suppress}` позволяет provider-у явно начать resource lifecycle без retained secret material. Existing full/range constructors по умолчанию остаются `ForwardScoped`; Suppress не извлекает headers/cookies/query, сохраняется через retries, а redirects никогда не восстанавливают forwarding. Smooth выводит intent один раз из effective manifest target; HLS/DASH behavior не изменён. Полный consumer contract: `mem:media-services/smooth-fragment-sources-s36p3-2026-07-25`.
- Manifest и media segment используют разные `SecretRequestPurpose`; explicit Cookie header запрещён, cookies живут только в scoped ephemeral jar. Raw URL/header/cookie/query не появляются в Debug/errors/tracing.
- `HttpRequestTarget::with_query_override` применяется только к scoped S21T query material; durable/persistence surfaces не добавлены.

## Focused proof

- Hermetic local servers покрывают effective redirect base URI, slow stale manifest refresh, same-generation no-side-effect rejection, manifest body bound, exact Range, transient Range retry, full-resource retry recovery/exhaustion, cancellation before request и во время retry backoff, overlapping live refresh dedup, component clock/live metadata replacement, cross-origin header/query stripping и deferred initial prefetch off player-owner.
- Финальный affected suite: `source-core` 27, `demux-api` 29, `web-media-adaptive` 14, `web-media-http` 21 unit/integration, `web-media-transport-api` 6 — PASS.
- Strict affected Clippy `-D warnings`, rustdoc `-Dwarnings`, Rust 1.92 workspace check, fmt, format/refactor guardrails и `git diff --check` — PASS.

## Следующие consumers / limitation

- S32 HLS VOD и S34 DASH VOD должны интерпретировать manifests в neutral snapshots и не копировать HTTP/retry/generation/secret policy.
- S31L владеет player-facing dynamic live/DVR timeline publication; S31 только хранит transport metadata.
- Seekable adaptive demux control, concrete live refresh cadence и endpoint refresh mapping не должны добавляться в этот crate без отдельного boundary decision.

Связанные memories: `mem:core`, `mem:media-services/core`, `mem:media-services/direct-media`, `mem:media-services/secret-safe-locators-s10b`, `mem:media-services/web-transport-s21t-2026-07-21`, `mem:media-services/progressive-http-s22-2026-07-22`, `mem:demux-api/core`.

## S12 active-read cancellation proof (2026-09-05)

- `tests/blocking_resource_fetch/cancellation_priority.rs` отдельно проверяет source/seek cancellation уже armed active HTTP body: consumer получает Cancelled дважды, bytes=0, socket закрыт, запрос один, независимый token не отменён, active slot освобождён.
- `restartable_read_interruption::test_observation` находится в `tests/restartable_read_observation.rs`, только cfg(test). Метод владельца `wait_until_network_read_is_active` read-only наблюдает phase; fixture держит body stalled до отмены. Deadline означает bounded failure, не успешную синхронизацию по времени. Production API/semantics не меняются.
