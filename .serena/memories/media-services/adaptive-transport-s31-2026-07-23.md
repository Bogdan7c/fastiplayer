# S31 — Adaptive transport foundation (2026-07-23)

## Итог

- Новый workspace crate `web-media-adaptive` владеет shared provider-neutral manifest/segment lifecycle для будущих HLS/DASH/ISM/HDS consumers.
- Concrete manifest/container parsing, yt-dlp DTO, UI, queue, player live timeline, ABR и encryption policy намеренно отсутствуют.
- Existing finite `demux_api::OrderedSegmentSource` не менялся: S28G Symphonia/MPEG-TS/FLV consumers сохраняют прежний contract.

## Ownership и boundaries

- `source-core::HttpSourceSession::fetch_bounded_single_hop` — единственный low-level owner blocking reqwest GET для bounded metadata/media resource: redirects остаются manual, full body ограничен caller bound, exact Range требует `206` + matching `Content-Range`, cancellation проверяется до request и между body reads. `200` для exact Range остаётся typed unsupported; остальные non-206 statuses сохраняются как `HttpStatus`, чтобы adaptive retry мог отличать transient 5xx/408/429.
- `web-media-adaptive::AdaptiveHttpContext` создаётся из S21T `TransportOpenRequest` и переиспользует exact `HttpRequestTarget`, `SourceGeneration`, expected `MediaPresentation`, `RedirectPolicy`, scoped `SecretRequestContext`, cookie jar и shared cancellation. Второго reqwest client/cache/prefetch stack нет.
- Manifest owner: `AdaptiveManifestFetcher` + `ManifestFetchRequest/ManifestPoll/ManifestResource/ManifestBaseUri`. Base URI всегда effective post-redirect target; relative references разрешаются через source-core WHATWG `HttpRequestTarget::resolve_reference`. Same/older claimed generation отклоняется до нового network side effect; slow stale outcome не публикуется.
- Segment owner: `AdaptiveOrderedSegmentSource` принимает bounded `AdaptiveSegmentSnapshot` и poll-ит `SegmentPoll::{Segment,TemporarilyUnavailable,EndOfStream,Failed,Cancelled}`. Readiness не кодируется как `None`, error, empty segment или fake EOF. Snapshot presentation обязан совпасть с S21T request. Same/older refresh rejected; overlapping newer live snapshot фильтрует уже delivered sequences, поэтому segment не отдаётся повторно.
- Full-resource segments ограничены `maximum_segment_bytes`; `SegmentByteRange` проверяет offset overflow и exact length. Retry policy typed/bounded: non-zero attempts, exponential capped delay, transient transport/body/timeout/status failures; cancellation и policy/secret/shape failures не retry-ятся.
- `AdaptivePresentation` даёт neutral VOD/live-edge/optional DVR vocabulary; `ComponentClockMetadata` хранится отдельно на каждый audio/video component и не смешивает timescale/origin.

## Player-owner / demux lifecycle

- `BlockingOrderedSegmentAdapter` — узкий bridge только для выделенного demux worker-а. Он может ждать poll readiness, но player-owner никогда не блокируется.
- `demux_api::ProgressiveDemuxer::new_deferred` переносит initial segment readiness, registry sniff/open и дальнейший parser loop на worker. До open player-facing `next_event` возвращает existing S21R `TemporarilyUnavailable`; после open первым публикуется `TracksChanged` и optional metadata.
- Deferred wrapper typed-отклоняет seekable inner вместо молчаливой потери seek contract. S31 не реализует VOD/DVR seek; это остаётся последующим transport/demux/player scope.
- Drop/cancellation не join-ит blocking network worker на player-owner. Уже выполняющийся blocking socket read, как и existing S22 path, освобождается на configured timeout/read boundary; poll boundary видит cancellation немедленно.

## Secret/redirect invariants

- Automatic redirects выключены. Каждый hop проходит S21T `RedirectPolicy`; cross-origin forwarding монотонно лишается header/query secret material.
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