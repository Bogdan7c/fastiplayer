# S22 — Progressive HTTP vertical slice (2026-07-22)

Связанные memories: `mem:core`, `mem:media-services/core`, `mem:media-services/direct-media`, `mem:media-services/web-transport-s21t-2026-07-21`, `mem:demux-api/core`, `mem:media-services/secret-safe-locators-s10b`.

## Production ownership

- Новый concrete crate `web-media-http` реализует S21T `TransportProvider` с provider ID `progressive-http`.
- Его normal dependencies намеренно ограничены только `source-core`, `media-prefetch`, `web-media-transport-api`. Это закреплено `scripts/check-refactor-guardrails.py`; прямые `reqwest`, demux, player и service dependencies запрещены.
- `source-core::HttpSourceSession` — единственный owner reqwest client-а для component open: automatic redirects выключены, каждый hop возвращается provider-у для S21T policy решения.
- Первый request всегда `Range: bytes=0-0`. Корректный `206` превращается в `HttpRangeSource` с тем же Client и существующим `media-prefetch`; `200` сохраняет уже открытый response как `HttpStreamingSource`, поэтому duplicate probe/download request отсутствует.
- Для seekable source `web-media-http` совмещает global `PrefetchConfig` с optional `TransportOpenRequest::http_range_request_limit()` через `min`: initial/chunk никогда не увеличиваются, window остаётся global memory policy. Поэтому yt-dlp `http_chunk_size` ограничивает каждый последующий Range request, но никак не влияет на non-Range streaming path.
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
- (Устарело с UX16, см. ниже: теперь и seekable output уходит в фоновый `ProgressiveDemuxer` через `media-source-open::progressive_player_demux`.)
- Progressive queue budgets выводятся из существующего prefetch window/chunk config: второго cache/prefetch policy нет.
- `service-ytdlp` остаётся extractor/descriptor owner и не зависит от `web-media-http`.

## Focused proof

- Hermetic tests: one-request non-Range body reuse, existing Range prefetch path, muxed/video/audio roles, redirect secret stripping, same-origin empty-secret redirect, redaction, cancellation, stale refresh и semantic mismatch before provider/network.
- Embedded tiny real fixtures открывают MP4, M4A и WebM по non-Range HTTP; separate MP4 video + M4A audio проходят через neutral `CompositeAvDemuxer`.
- `demux-api` tests доказывают non-blocking player read, bounded oversized packet failure и cancellation/backpressure on drop.
- `service-direct-media` tests фиксируют Range/non-Range parity.


## S23 yt-dlp production consumer (2026-07-22)

- `app-egui::web_media_open` is now the yt-dlp composition root over the same S22 `WebMediaHttpProvider`, `TransportRegistry`, `DemuxRegistry` and progressive wrapper used by the direct-media vertical slice.
- `service-ytdlp` maps S19 candidates into neutral S21C planning data and S21T requests, but still has no concrete HTTP/demux dependencies. Its legacy WebM-only opener and direct reqwest/prefetch/demux stack are deleted.
- S26 authorization mapping is not guessed: candidates requiring headers/cookies return typed pre-barrier `AuthorizationMappingPending` without dropping secret material. Full flow: `mem:app-egui/queue-owned-web-open-s23-2026-07-22`.

## S27 read-time Range redirects (2026-07-22)

- Initial redirects and redirects returned by later seekable Range reads share the same transport-owned `RedirectPolicy`/`SecretRequestContext` semantics. Reqwest automatic redirects remain disabled.
- `source-core::HttpRangeSource` owns physical Range mechanics, parses each `Location`, counts each physical request and checks cancellation before every hop. Every logical read and retry starts from immutable stable base target/headers/body; redirected material is local to that chain.
- `web-media-http::ScopedRangeRedirectHandler` owns only redirect policy, ephemeral secret context and sticky per-read forwarding state. Cross-origin transitions monotonically strip headers/cookies/body; `301/302/303` monotonically switch to GET. `source-core` independently prevents a later `307/308` from resurrecting an already dropped body.
- Focused proof covers stable-base restart across repeated reads, `POST -> 302 -> 307` body non-resurrection, per-physical-request diagnostics and a real prefetch cross-origin redirect that reaches final `206` without Authorization/initial Cookie/Set-Cookie leakage.


## HTTP client identity и error taxonomy hardening (2026-08-10)

- `source-core/src/http_client.rs` — единый owner common blocking Reqwest builder-а для `HttpRangeSource` и `HttpSourceSession`, включая initial probe, последующие Range reads и adaptive helper paths. Builder задаёт connect/read timeout и публичный descriptive User-Agent `fastiplayer/<version> (https://github.com/Bogdan7c/fastiplayer)`.
- User-Agent — публичная идентичность клиента, а не credential: он не хранится в `SecretRequestContext`, не участвует в credential forwarding и может быть осознанно переопределён explicit request header. Wikimedia-подобные endpoints отклоняют пустой UA; hermetic tests теперь проверяют общий UA на probe и Range requests.
- HTTP 401 и proxy 407 остаются `ProviderError::Authentication`; обычный 403 без authentication challenge отображается в новый typed `TransportFailure::AccessDenied`. Запрещено превращать 403 в выдуманный `CredentialsMissing`.
- Acceptance row 05 `Big_buck_bunny_720p_5mb.webm` проверен end-to-end: seekable Range open, WebM demux, VP9 render, six-channel Opus decode/mix и drain до конца.

## UX16 — переподключение после обрыва сети (2026-10-08)

План: `user/ux-edge-cases/16-progressive-http-reconnect.md`, итог: `user/ux-edge-cases/results/16.md`.

- Корень «обрыв = Failed навсегда» был в трёх слоях: (1) один мгновенный retry в `HttpRangeSource`; (2) `PrefetchingByteSource::read` отдавал fatal error worker-а раньше уже буферизованных байт; (3) seekable HTTP-demuxer работал синхронно на потоке player-а и блокировал tick в prefetch wait (Buffering не включался — открытый риск сессии 15).
- (1) Политика повторов (решение владельца): паузы 0,5→8 с, бюджет `network.reconnect_wait_ms` (default 60 с), `Retry-After` cap 60 с, только transient (сеть, 5xx/408/429), продолжение с первого недостающего байта, сверка representation → `HttpRepresentationChanged`. Детали — `mem:source-core/core` (UX16).
- (2) `media-prefetch`: буфер раньше ошибки (ошибка относится к fetch-границе окна); после чтения ошибки worker повторяет fetch с той же границы.
- (3) `media-source-open::progressive_player_demux::into_player_demuxer(demuxer, TransportSeekability, cancellation, prefetch_config) -> ProgressivePlayerDemuxer { demuxer, seek_port }`: seekable → `ProgressiveDemuxer::new_receipted_seekable` (generation 1, 16 receipts, retry hint 10 мс) + `ProgressivePreparedDemuxSeekPort`; streaming → прежний `ProgressiveDemuxer::new`. У seekable runtime синхронный `seek_with_request` отключён (`seek_controller: None`) — перемотка ТОЛЬКО через seek port, поэтому порт обязан дойти до player-а (`PreparedWebMediaSeekAttachment::WorkerReceipted`). Direct: `DirectProgressiveOpenResult::into_runtime_parts() -> DirectProgressiveRuntimeParts { demuxer, demux_seek_port, endpoint_recovery }` (порт обёрнут `VodEndpointRecoveryAttachment::wrap_seek_port`). yt-dlp progressive (`web_media_open/runtime.rs`): если есть seekable компонент, в фон уходит весь собранный (composite) demuxer; forward-only компоненты по-прежнему обёрнуты поштучно.
- Закрытие во время обрыва: Symphonia читает с вечным токеном, поэтому `web-media-http` привязывает `PrefetchingByteSource::with_lifecycle_cancellation(request.cancellation())`; отмена (drop `ProgressiveDemuxer`) будит ждущее чтение `SourceError::SourceClosed` (не `Cancelled`→`Interrupted`, который Symphonia повторяет бесконечно), worker выходит, Drop prefetch отменяет fetch и паузу повтора.
- «Нет сети» для UI: `symphonia-demux::byte_source` и `demux-api::StreamingSourceByteReader` мапят transient `SourceError` в `io::ErrorKind::NetworkDown`; player-core → `PlayerErrorKind::NetworkError`; текст — `mem:app-egui/notifications` (UX16).
- Тесты: сквозные `media-source-open/src/direct_progressive_open/network_drop_tests.rs` (WAV через `open_direct_media`, обрыв 3 с с refused connections → байты и PCM production-декодера совпадают, `next_event` < 250 мс; бюджет → `NetworkDown`; закрытие → нет новых подключений), граница `progressive_player_demux/tests.rs`, prefetch `source/tests/{buffered_before_error,lifecycle_cancellation}.rs`. app-egui direct HTTP/FTP тесты перематывают через seek port (`seek_through_port`; FTP-Ogg seek ~10 с: бинарный поиск Ogg × новое FTP-соединение на каждый byte seek — так было и синхронно).
- Ёмкость очереди событий фонового demuxer-а — константа `PROGRESSIVE_EVENT_QUEUE_CAPACITY = 256` (байты = prefetch window), НЕ «window / chunk»: при `read_ahead_mb = prefetch_chunk_mb` старая формула давала очередь на 1 пакет, каждый проход player-а кончался `TemporarilyUnavailable`, и незакрытый demux-retry навсегда блокировал выход из Buffering (найдено ручной приёмкой UX16; регресс `progressive_player_demux/tests.rs::tiny_prefetch_window_still_queues_a_batch_of_packets`). yt-dlp forward-only компоненты (`web_media_open/runtime.rs::progressive_limits`) ещё на старой формуле.
- Живая приёмка UX16 (archive.org BBB, `nmcli radio wifi off/on`): короткий обрыв (< read timeout 15 с) переживает сам TCP; длинный → повторы → Buffering → восстановление с той же позиции; смена файла во время спиннера мгновенна и отменяет повторы; бюджет → «Нет связи…». Решение владельца 2026-10-09: после сетевого `Failed` очередь ОСТАНАВЛИВАЕТСЯ (не skip): `EndedSnapshotKind::ErrorAssociated { cause: PlaybackFailureCause::{MediaFault, NetworkLost} }` (классификация в `app-egui/playlist_runtime/discovery/navigation.rs::automatic_snapshot_kind` по `PlayerErrorKind::NetworkError`) → `AutomaticStopCause::NetworkLost`, без runtime-бейджа и без skip-notice. Не покрыто (бэклог): отказ ОТКРЫТИЯ следующего элемента без сети всё ещё идёт skip-политикой — в `report_*target_failure` доходит только текст, не `WebOpenFailureReason`.
- Известные ограничения: сервер без Range (200) не переподключается; seek во время обрыва ждёт сеть (нет `active_read_interruption` у Symphonia поверх prefetch); после возвращения сети player ждёт докачки остатка текущего prefetch-куска (до 8 МБ) — заметно только на медленной сети.
