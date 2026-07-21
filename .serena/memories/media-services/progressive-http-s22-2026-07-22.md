# S22 — Progressive HTTP vertical slice (2026-07-22)

Связанные memories: `mem:core`, `mem:media-services/core`, `mem:media-services/direct-media`, `mem:media-services/web-transport-s21t-2026-07-21`, `mem:demux-api/core`, `mem:media-services/secret-safe-locators-s10b`.

## Production ownership

- Новый concrete crate `web-media-http` реализует S21T `TransportProvider` с provider ID `progressive-http`.
- Его normal dependencies намеренно ограничены только `source-core`, `media-prefetch`, `web-media-transport-api`. Это закреплено `scripts/check-refactor-guardrails.py`; прямые `reqwest`, demux, player и service dependencies запрещены.
- `source-core::HttpSourceSession` — единственный owner reqwest client-а для component open: automatic redirects выключены, каждый hop возвращается provider-у для S21T policy решения.
- Первый request всегда `Range: bytes=0-0`. Корректный `206` превращается в `HttpRangeSource` с тем же Client и существующим `media-prefetch`; `200` сохраняет уже открытый response как `HttpStreamingSource`, поэтому duplicate probe/download request отсутствует.
- Request body выражен typed `HttpRequestBody`; `307/308` сохраняют method/body, `301/302/303` переключают последующие hops на GET без body.
- S21T `SecretRequestContext` извлекается только после scope проверки. Cross-origin redirects strip credentials; same-origin redirect без секретов разрешён даже вне пустого secret scope. URL/header/body не попадают в Debug/errors.
- Refresh проходит только через S21T exact semantic identity и source-generation fences; mismatch/stale cancellation отсекаются до network side effect.

## Demux/player lifecycle

- `demux-api::DemuxInput::streaming_source` адаптирует S21T forward-only source к concrete blocking factory.
- Нельзя передавать `WouldBlock` из середины Symphonia container parse: parser может уже потребить часть элемента. Поэтому `demux-api::ProgressiveDemuxer` владеет отдельным blocking demux worker-ом.
- Player-facing `next_event` никогда не ждёт inner demuxer: читает bounded queue либо возвращает `TemporarilyUnavailable`.
- Queue ограничена одновременно количеством событий и encoded bytes. Oversized packet даёт typed error; full queue создаёт backpressure worker-у.
- Drop выставляет stop, cancellation и будит Condvar. Join намеренно не выполняется на player owner-е: blocking network read завершается на cancellation/read-timeout boundary.
- RAII completion guard помечает worker stopped даже при panic backend-а, чтобы player не ждал бесконечно.
- Progressive input всегда сохраняет исходную typed non-seekable причину; seek запрещён.

## Direct-media integration

- `service-direct-media` сохраняет прежние classification/locator/privacy contracts, но open adapter использует `web-media-http` через `TransportRegistry`, затем `DemuxRegistry` + `SymphoniaDemuxFactory`.
- Adapter передаёт одновременно real extension и normalized container hint: MP4/MOV -> `iso-bmff`, MKV -> `matroska`, WebM -> `webm`.
- Seekable Range output идёт напрямую в demux; Streaming output оборачивается в `ProgressiveDemuxer`.
- Progressive queue budgets выводятся из существующего prefetch window/chunk config: второго cache/prefetch policy нет.
- `service-ytdlp` остаётся extractor/descriptor owner и не зависит от `web-media-http`.

## Focused proof

- Hermetic tests: one-request non-Range body reuse, existing Range prefetch path, muxed/video/audio roles, redirect secret stripping, same-origin empty-secret redirect, redaction, cancellation, stale refresh и semantic mismatch before provider/network.
- Embedded tiny real fixtures открывают MP4, M4A и WebM по non-Range HTTP; separate MP4 video + M4A audio проходят через neutral `CompositeAvDemuxer`.
- `demux-api` tests доказывают non-blocking player read, bounded oversized packet failure и cancellation/backpressure on drop.
- `service-direct-media` tests фиксируют Range/non-Range parity.
