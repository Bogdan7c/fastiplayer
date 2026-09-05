# HLS VOD manifest-owned worker-receipted seek (2026-08-24)

## Подтверждённая причина

- На реальном `https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8` late video seek попадал в `ProgressiveSeekCommand::Previewed`/ordinary `Demuxer::seek_with_request`, хотя static HLS уже передавал `PreparedDemuxSeekPort` через app `PreparedMedia`.
- Причиной был `PlayerSession::start_one_shot_seek_landing`: после S17A video one-shot безусловно выбирал reused-decoder scrub route, который обязан выполнять synchronous preview-compatible demux seek. Audio-only использовал generic seek transaction и receipt port, поэтому старые receipt tests не видели video regression.
- Старый HLS observed index после раннего playback имел только RAP около 0 s; ordinary seek поэтому переоткрывал initial suffix и последовательно читал/декодировал timeline до target. Последующий generic `DemuxError` был downstream следствием долгой replacement работы, а не корнем.

## Boundary и ownership

- `media_core::Demuxer::seek_with_receipted_request` — новый additive default boundary. Default делегирует `seek_with_request`, поэтому все legacy/local demuxers сохраняют поведение.
- `demux-api` вызывает этот boundary только для `ProgressiveSeekCommand::Receipted`. Previewed command продолжает ordinary path и сохраняет exact synchronous preview/worker-result parity.
- `PreparedDemuxSeekRuntime::routes_one_shot_seek_through_worker()` скрывает mode storage. Video one-shot с worker-receipted capability выбирает существующую generic async seek transaction; legacy video без порта и live scrub сохраняют cold reused-decoder route.
- HLS immutable `HlsComponentPlan` владеет отсортированными manifest segment start points. Receipted seek пробует containing target segment, затем previous segment только внутри того же epoch/discontinuity.
- `epoch_demux/manifest_seek.rs` готовит replacement offside, динамически доказывает настоящий video RAP/audio packet в существующих event/byte budgets, публикует только доказанный anchor и при отсутствии безопасного candidate откатывается к прежнему exact observed-anchor path.
- `HlsSeekAnchor::timeline_origin` сохраняет правильный presentation origin для anchor, впервые доказанного из segment suffix-а.
- Separate A/V по-прежнему готовится транзакционно. Replacement audio component подавляет только второй redundant `TracksChanged`; иначе generic composite reset очищал уже pending video RAP. Initial composition этот suppression не использует.
- Static live/DVR HLS evidence owner и refresh/expiry semantics не изменены.

## Regression anchors

- `crates/web-media-hls/tests/receipted_manifest_seek.rs`:
  - late muxed seek fetch-ит target segment и пропускает все промежуточные;
  - target/previous без RAP сохраняют successful exact-anchor fallback;
  - separate A/V готовит near-target video+audio pair и публикует оба landing packets со stable topology.
- `crates/player-core/src/session/tests/prepared_demux_seek.rs` доказывает, что public video `PlayerCommand::Seek` создаёт worker request, не вызывает active synchronous demux seek, authoritative receipt запускает commit, а post-seek packet проходит decoder и доходит до target-frame presentation.
- Legacy video without port test сохраняет synchronous seek. Demux-api tests отдельно доказывают, что receipted command не протекает в ordinary boundary.
- Exact manifest-boundary и discontinuity tests находятся в `web-media-hls/src/plan.rs`.

## Проверка первоначальной границы

- Initial affected Rust suites, strict Clippy, workspace check, formatting/diff and refactor guardrails passed when the worker-receipted boundary was introduced.
- Hermetic functional tests proved target-segment selection, exact-anchor fallback, separate A/V topology and player delivery through decoder/presentation. Текущий comprehensive release/real snapshot находится в финальном разделе ниже.

## Follow-up: UI scrub release (2026-08-24)

- Реальный timeline drag использует `BeginScrub -> PreviewScrub -> EndScrub`. Matching live preview landing нельзя коммитить как final для worker-receipted runtime: release обязан запустить authoritative final async transaction и отключить progressive pre-target presentation.
- Functional regression `worker_receipted_scrub_release_suppresses_preroll_until_target_frame_presentation` проходит production command chain, receipt, demux packets, decoder и presentation. Он доказывает один final worker request, отсутствие второго sync seek и отсутствие видимого pre-target frame после release.
- Legacy/local media без worker receipt сохраняет synchronous preview-compatible behavior. Финальный реальный drag/cancellation snapshot приведён ниже.

## Source-scoped landing policy и подтверждённый финальный state (2026-08-28)

### Граница policy

- `web-media-hls::HlsVodSeekLandingPolicy` принадлежит HLS open boundary: default `DecodeFromOrBeforeTarget`, явный opt-in `PreferPostTargetRap`.
- Единственный production opt-in находится в `app-egui::web_media_hls_open::prepare_native_hls_vod`. Shared yt-dlp HLS VOD использует default decode-forward policy, live HLS остаётся на отдельном legacy/live demuxer path. URL heuristics и positional bool отсутствуют.
- Policy проходит до initial open/restore и worker-receipted manifest seek. Если post-target RAP не найден или не доказан в bounded budget, native opt-in сохраняет decode-forward fallback.
- HLS выбирает manifest segment и packet-derived actual/decode anchor. Player отдельно владеет receipt interpretation, decoder/presentation/audio readiness и final position commit; receipt сам по себе seek не завершает.

### Cancellable preview/receipt lifecycle

- `Demuxer::seek_with_cancellable_preview_request` сохраняет preview semantics, но проводит request-scoped `DemuxSeekCancellationToken` до HLS transport body. Новый final receipt физически отменяет superseded preview, а single worker не ждёт stale body.
- Component и separate A/V replacement готовятся offside. Один shared token для video+audio должен выиграть `complete()` до active-read activation, staged index/marker commit и atomic swap. На cancellation/failure старый committed source или A/V pair остаётся authoritative.
- Packet-proven anchor не попадает в shared `HlsSeekIndex` во время prepare: evidence вставляется только в authorized commit section. Отмена на video или audio phase не меняет следующий preview selection.
- Encrypted media и external keys дочитываются через HLS-owned bounded cancellable streaming helper; partial ciphertext/key не публикуются и не попадают в key cache. Drop response body физически освобождает старый HTTP stream.
- InitialOpen/InitialRestore marker создаётся из packet-derived anchor и публикуется только после topology validation, active-read commit и final muxed/A-V assembly. Separate A/V cold pair коммитится атомарно.

### Secret-safe committed-selection diagnostics

- `HlsManifestSegmentSeekMarker` публикуется через neutral `log` facade на INFO target `fastiplayer::hls_manifest_selection`; concrete logger остаётся composition-root ownership.
- Marker хранит phase, component role, opaque HLS-local selection ID, landing policy, source generation, requested target, actual/decode anchor и kind, media/discontinuity sequence, global/epoch/restart indexes и half-open segment interval. URI/query/header/cookie/token/key/map/hash/cache/resource/request IDs отсутствуют.
- `scripts/playback_acceptance_hls.py` разбирает exact Display schema независимо от public seek correlation. ID opaque и unique per log source; числовая monotonic order не является schema invariant. Strict analyzer fail-closed проверяет enum/domain/interval/anchor/duplicate anomalies.

### Functional proof

- media-core закрепляет default delegation, pre-cancel и semantic split preview/receipt; demux-api causal worker test доказывает старт final receipt после cancellation in-flight preview без manual release.
- HLS loopback tests физически закрывают partial plaintext, ciphertext и rotated-key bodies; final receipt продолжает работу без stale packet/cache publication.
- Separate A/V production-like loopback отменяет partial video и partial audio candidate, затем читает старый non-EOF committed pair со stable IDs/no `TracksChanged`, и только отдельный успешный receipt делает один atomic commit.
- Shared-index tests закрепляют stage/drop/authorized-commit; cold open/restore tests покрывают post-target, fallback, discontinuity и failure-no-marker.
- Player functional tests доводят actual landing до decoder, video presentation, audio readiness, commit и post-landing progress; stale generation, paused intent и readiness failures не коммитятся.

### Финальный release/real acceptance snapshot (2026-08-28)

- Проверен release binary из clean committed HEAD `72a3cbf7` (`fix(hls): cancel superseded previews and report landings`); SHA256 release snapshot: `4c03f566e02296545796f79c17dda1ace82449c66d3b19b9aeecdfcca7f27c85`.
- 3 cold InitialOpen: process-to-ready `609/624/492 ms` (p50 `609`, max `624`), actual/decode anchor `33/0 ms`, segment `[0,10000)`.
- 3 cold InitialRestore requested `355.000 s`: process-to-ready `702/447/573 ms` (p50 `573`, max `702`), actual/decode anchor `360.033/360.000 s`.
- 10 warm final seeks: ready `33/483/851/1169/31/549/627/241/338/34 ms`, p50 `338`, p95/max `1169`. Единственный residual `1169 ms` относится к внешней body delivery до receipt: headers/first byte `101 ms`, enqueue-to-receipt `1147 ms`, затем receipt-to-video/audio/commit `18/19/19 ms`; повтор того же 550 s path дал `241 ms`. Это не доказательство post-receipt regression и не обещание network latency.
- 3 process-restart final seeks завершились за `223/245/255 ms`.
- 3 causally gated rapid `550 -> 60 s`: каждый old 550 request вошёл в worker/HTTP и физически завершился cancelled за `7/8/11 ms` без old receipt/frame/audio/commit/progress; каждый winning 60 завершился за `28 ms`.
- 3 настоящих KWin EIS timeline drag-а дали `4/7/7` preview dispatches и final requested/actual `355.031/360.033 s`; final ready `237/35/28 ms`. UI screenshots и telemetry подтверждают video surface/render, audio play и последующий progress.
- Strict aggregate: 12 startup runs, 19 completed final seeks и 3 честных superseded old seeks; HLS/network/scrub proof anomalies 0. Committed marker counts: InitialOpen 3, InitialRestore 9, Preview 9, FinalReceipt 19; privacy scan clean.
- Доказанных `output_ring_underrun_proven_by_silence_padding` событий 0. Три drag-а дали risk-only observations с `new_silence_padding_callbacks=0`; они не скрываются, но не являются доказанным underrun.
- Реальный x36 media playlist не содержит discontinuity: все 40 marker records имели `discontinuity_sequence=0`. Discontinuity/cross-epoch correctness подтверждается synthetic loopback/integration tests, а не приписывается реальному CDN stream.
- Controlled profile после каждого набора и финально был восстановлен byte-for-byte; ephemeral backup locations не являются project contract и в memory не хранятся.

### Финальная проверка и repository state

- PASS: full three-crate all-target/all-feature tests, HLS integration matrix, focused/repeated cancellation cases, strict affected Clippy `-D warnings`, workspace formatting/diff, refactor/S42/format guardrails, Python HLS analyzer tests, release build и strict real-log analyzer.
- Canonical HLS change закоммичен как `72a3cbf7`; refactor/S42/format guardrails прошли, после acceptance worktree clean.
- Абсолютные test counts и external-network latency не являются долговечным контрактом; при следующих изменениях читать конкретный CI/acceptance run.

Related: `mem:core`, `mem:player-core/core`, `mem:demux-api/core`, `mem:media-services/hls-preview-receipt-cancellation-2026-08-27`, `mem:media-services/hls-manifest-selection-diagnostics-2026-08-27`, `mem:media-services/hls-ts-resource-bounded-initial-probe-2026-08-24`, `mem:media-services/hls-vod-seek-index-compaction-aud010-2026-08-23`.
