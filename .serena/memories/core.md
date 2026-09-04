## Public launch S05: public-safe AI tooling (2026-09-05)

- `AGENTS.md` переведён на английский с сохранением правил. Owner уточнил: русские комментарии нужны к ключевой логике, неочевидным решениям и инвариантам, не к каждой строке. См. `mem:conventions`.
- `.serena/project.yml` использует `rustiplayer`; fresh MCP activation по пути и имени проверена. Старый работающий MCP server кэширует прежнее имя до reconnect/restart.
- Codex hooks сохранены как optional maintainer tooling и отключены по умолчанию для fresh clone; opt-in и внешние зависимости описаны в `docs/ai-development.md`. Cargo/CI не зависят от AI tooling.
- Personal paths и owner-local media filenames заменены описательными placeholders; технические/root-cause/codec сведения сохранены. Current workflow и семь standalone patches: `mem:task_completion`, `mem:suggested_commands`. Итоги и границы current-tree scan: `mem:public-launch/s05-public-ai-tooling-2026-09-05`.

## Public launch S02: module boundary splits (2026-09-04)

- `app-egui/state.rs` и `web-media-dash/discovery.rs` снова проходят size guardrails после refactor-only split-а.
- Startup context/wake bridge теперь в `state/startup_context.rs`; provider DASH lane proof/validation/error mapping — в `discovery/lane_proof.rs`. Public API, playback/error/selection/accounting semantics не менялись.
- Focused app/player/DASH tests, пять native DASH consumer verticals, guardrails, strict Clippy, Rust 1.96 workspace check, fmt/diff и Serena audit прошли. Полный handoff: `mem:public-launch/s02-module-boundary-split-2026-09-04`.

# Public launch S00: private user-docs extraction (2026-09-04)

- `user/` удалён из tracked tree и защищён корневым `/user/` ignore; приватные документы сохранены вне репозитория в mode-0700 timestamped backup, включая recoverable original и opaque copy.
- Pre-S00 Git state сохранён full `git bundle --all` с 45 refs, stash и 35 registered worktree HEADs; verify/list-heads/mirror-clone/fsck/checksums прошли.
- Commit: `a57a29d28015f2f74e095d1841cabbf39704035e`, parent `c2adea8bcbfa6fbd546231b076c5359790288aee`; history rewrite/push не выполнялись. Полный handoff: `mem:public-launch/s00-user-backup-2026-09-04`.

## Native network fast-start / responsive seek (2026-09-04)

- HLS x36xhzz regression root cause was full first-TS download per catalog sibling plus stale preview anchor reuse. Catalog now uses bounded pull-stream TS proof with fMP4-only fallback; native final seek uses fresh manifest reanchor and containing-segment decode-forward. Final public cold open ~1.61 s vs ~18.3 s, +120 s seek 448 ms to correct frame with zero pre-target presentation.
- DASH root causes were sequential lane/catalog proof, serialized A/V preparation, and one HTTP Range RTT per demux packet. Catalog proof is bounded-parallel (app 16), A/V open/seek is scoped-parallel and atomic, and SegmentBase owns 64 KiB latency / 1 MiB throughput / two-page read-ahead plus completed-only shared VOD replay. Public +30 s seek ~1.50 s, of which 1.467 s is the single CDN Range request and 33 ms is decode/present.
- Smooth A/V preparation is scoped-parallel and atomic. Automatic quality starts at 720p, upshifts only after stable evidence, downshifts on underrun/750 ms buffering, and never changes strict manual height preference or persists an adaptive step. Full invariants, tests, external caveats and commands: `mem:media-services/native-network-fast-start-responsive-seek-2026-09-04`.

## G3 native-ingress final qualification (2026-09-02)

- G3 architecture/security audit подтвердил единый neutral web active-source/catalog plane, exact process set `{row00,row08}`, zero spawn для 11 direct rows при `yt_dlp.enabled=false`, pre-Installed-only fallback и отсутствие persisted/logged temporary secrets/endpoints.
- Добавлен отсутствовавший functional cross-source queue proof `HLS → DASH → Smooth → DASH`: каждый transition достигает video decode/WGPU readback + nonzero PCM до queue commit, process spy 0. Тест покрывает media/queue boundary, но не windowed AppState/UI Next; для точного remaining UI issue нужна конкретная row pair + symptom.
- Gate исправил owner-level DASH ignored-subtitle validation, восстановил neutral decode-start ownership (`media-core` evidence, H.264 single-pass classifier) и закрыл пять test/coverage gaps: adaptive exposed prefix, playlist worker disconnect, pre-cancelled preparation, stale mismatched progressive seek и playback-intent wake → exact session/snapshot consumer. Финальный coverage baseline квалифицирован exact 9/9 intersection и двумя fresh PASS; hashes/counts и discarded-attempt audit находятся в `mem:testing/native-web-ingress-g3-2026-09-02` и `mem:testing/coverage`.
- Public N15 acceptance: 11 PASS, rows 04/12 PROFILE_EXCLUDED, process set `{00,08}`, настоящий HDR display NOT RUN; HDR→SDR evidence PASS. Новую feature-wave после G3 не начинать.

## N14B cross-protocol lifecycle matrix (2026-09-02)

- Focused `n14b_lifecycle` cohort даёт 17/17 functional proofs для VOD/live seek, Playing/Paused switch, queue Previous/Next/EOF/no-false-live-EOF, graceful close/restart/restore, recovery, persistence correlation и stale generation fences; каждый successful media transition снова достигает WGPU frame/readback/release или nonzero PCM.
- N14B обнаружил и исправил owner-level HLS TS DVR defect: после production decoder flush IDR без in-band SPS/PPS больше не публикуется как seekable decode anchor. Новый codec-core probe требует ordered SPS -> PPS -> IDR; правило применяется только в web-media-hls live H.264/TS evidence, общий MPEG-TS index не менялся.
- Direct/native extractor accounting остаётся structural/injected exact 0. Three-run 17/17 cohort, N14A 10/10 regression, codec/HLS/TS owners, strict Clippy, workspace check и fmt/diff прошли. Полный handoff: `mem:testing/native-web-ingress-n14b-2026-09-02`. Следующая session — N15, не начиналась.

## N14A hermetic protocol consumer matrix (2026-09-01)

- Focused `n14a_consumer` cohort покрывает все 12 direct/HLS/DASH/Smooth/HDS/extractor-page rows; video достигает WGPU submit/readback/release, audio — nonzero PCM и advancing production clock.
- Existing loopback origins теперь дают exact request/RETR/root response-body accounting; native adaptive injected process spies остаются 0, extractor page использует exact один production `PageMediaResolution` attempt.
- Product/API/config/persistence не менялись; three-run cohort и focused owner/DTO ratchets, strict Clippy и workspace check прошли. Полный handoff: `mem:testing/native-web-ingress-n14a-2026-09-01`. Следующая session — N14B, не начиналась.

## G2 native-ingress qualification (2026-09-01)

- Accumulated N06–N13B прошёл gate-only self-review, hermetic direct/HLS/DASH/Smooth/HDS verticals, `yt_dlp.enabled=false`, exact process-spy, request accounting, security/persistence/provider DTO ratchets, full pre-PR и release build.
- Gate обнаружил и исправил только root causes: S42 module/dependency drift и четыре test/coverage fixture gaps; production feature semantics не расширялись. Stable coverage принят exact 9/9 intersection, empty ledger и двумя fresh repeatability PASS.
- Exact hashes/counters/process set/test locations: `mem:testing/native-web-ingress-g2-2026-09-01`; authoritative baseline: `mem:testing/coverage`. Следующая session — N14A, в G2 не начиналась.

## N13B extractor invocation/redaction/persistence ratchet (2026-09-01)

- App web open/reopen attempts теперь реально несут один injected `YtDlpExtractorAdapter`; native fallback и extractor-backed HLS/DASH refresh не создают скрытый Default launcher, поэтому process spy наблюдает production boundary.
- Exact source ratchets фиксируют единственный production `Command::spawn`, candidate/topology owned-launcher entrypoints и полный app allowlist provider DTO; active source/playlist DTO shapes запрещают ephemeral endpoint/header/cookie material.
- 11 direct rows подтверждены existing functional verticals с честным zero-spawn proof и classifier-before-open request accounting; две page fixtures дают exact `PageMediaResolution` и один разрешённый spawn. Полные boundaries/tests/§6.3: `mem:media-services/native-web-ingress-n13b-2026-09-01`. Следующая session — G2.

## N13A source-owned cross-protocol recovery/fallback closure (2026-09-01)

- HLS/DASH/Smooth/HDS публикуют только neutral `WebMediaFallbackTrigger`; единый app-owned `NativeWebFallbackOwner` сохраняет exact extractor reason, допускает максимум один allowlisted pre-Installed attempt и fail-closed запрещает весь post-Installed fallback.
- Cancellation/network/malformed/expired/backpressure/invariant/decoder/render terminal; DASH DRM terminal. Page/extractor rows сохраняют locator и выполняют fresh extraction + semantic rematch только как `ExtractorBackedRecovery`.
- Direct HTTP/FTP получили armed endpoint recovery attachment; все reconstructible owners reopen-ятся от stable root, а ownerless temporary endpoint даёт typed terminal error.
- Полные boundaries, matrix/vertical evidence, §6.3 и известный no-default test-feature gap: `mem:media-services/native-web-ingress-n13a-2026-09-01`. N13B не начинался.

## N12 native HDS/F4M VOD без yt-dlp (2026-09-01)

- Direct HTTP(S) `.f4m` теперь admitted по syntactic path hint и authoritatively подтверждается existing F4M parser; первый bounded root response передаётся existing HDS discovery/runtime без второго GET.
- Neutral coupled catalog, exact semantic selection, worker-receipted VOD seek, switch/reopen и stable-root refresh используют existing HDS/F4F owners; durable source хранит только stable root/lineage/selection, process spy остаётся 0.
- Initial fallback разрешён только для foreign XML root или HTTP 401/403 auth; malformed/profile/DRM/private/live/network/cancel остаются distinct typed terminal errors без fallback.
- Hermetic three-run vertical достигает F4F -> H.264 FFmpeg -> WGPU submit/release и AAC -> nonzero PCM с exact root/Frag1 request accounting. Полные boundaries, tests, §6.3 и limitations: `mem:media-services/native-hds-vod-n12-2026-09-01`. N13A не начинался.

## N11 native Smooth Streaming VOD без yt-dlp (2026-09-01)

- Direct HTTP(S) `/Manifest` теперь admitted по syntactic path hint и authoritatively подтверждается existing Smooth parser/static H.264/AAC profile; первый bounded root response передаётся existing discovery/runtime без второго GET.
- Neutral catalog, exact semantic component selection, receipted VOD seek, switch/reopen и stable-root recovery используют существующий Smooth runtime; durable source хранит только stable root/lineage/selection, а process spy остаётся 0.
- Initial fallback разрешён только для foreign root или HTTP 401/403 auth; network/cancel/malformed/live/DRM/private/unsupported codec/native profile остаются distinct typed terminal errors без fallback.
- Hermetic three-run vertical достигает FFmpeg H.264 -> WGPU submit/release и AAC -> nonzero PCM с exact root request accounting. Полные boundaries, tests, §6.3 и limitations: `mem:media-services/native-smooth-vod-n11-2026-09-01`. N12 не начинался.

## N10 native dynamic DASH live/DVR без yt-dlp (2026-09-01)

- Supported direct dynamic MPD теперь проходит native fetched-body classification/catalog и existing S35 runtime без extractor; first root GET не повторяется, а stale fetched bytes не replay-ятся после initial open.
- Stable-root refresh, publish ordering, logical rematch, S31L DVR/expired seek и paused recovery сохранены; unsupported addressing остаётся typed profile exclusion, malformed/network/cancel — terminal no-fallback.
- Hermetic fMP4 vertical достигает H.264 FFmpeg/WGPU и AAC nonzero PCM, доказывает window shift, endpoint recovery, semantic reopen и process spy 0. Durationless RAP evidence завершается только следующим observed timestamp-ом.
- Полные boundaries, tests, §6.3 и limitations: `mem:media-services/native-dash-live-n10-2026-09-01`. N11 не начинался.

## N08 native HLS live/DVR без yt-dlp (2026-09-01)

- Supported sliding-live и EVENT HLS теперь admitted native и идут через existing `web-media-hls::live` S33 runtime; blanket `LiveOrEventPlaylist`/`LiveRequiresExtractor` vocabulary удалена.
- App native HLS composition типизирует VOD/live lifecycle: live получает dynamic timeline + worker-receipted DVR seek + stable-root endpoint refresh, VOD сохраняет initial position и отдельный endpoint recovery.
- Hermetic vertical доказал moving H.264 decoder/WGPU frames, AAC nonzero PCM, window shift, retained/expired DVR seek, endpoint recovery, semantic TS->fMP4 Playing/Paused switch и process spy 0; HE-AAC alternate row изолируется локально.
- Local commit `feat(hls): admit live manifests without yt-dlp`; полные boundaries, failure semantics, tests и §6.3 handoff: `mem:media-services/native-hls-live-n08-2026-09-01`. N09 не начинался.

## N07 native HLS VOD catalog/switch/reopen (2026-09-01)

- Existing native HLS VOD path теперь публикует полный neutral TS/fMP4 component catalog и проходит initial/switch/reopen через один FetchedTop handoff без extractor для valid supported finite profile.
- Fresh master ordinal используется только внутри текущего catalog snapshot-а; durable/app state хранит stable root lineage + semantic selection. Root refresh/recovery rematch-ит selection после перестановки rows.
- Catalog exact reopen теперь реально переносит caller start в receipted seek; hermetic vertical достигает H.264 decoder/WGPU и AAC nonzero PCM на fMP4 и TS, root GET exact 1/attempt, process spy 0, Playing/Paused lifecycle остаётся единым.
- Полные boundaries, root causes, tests, commands и limitations: `mem:media-services/native-hls-vod-n07-2026-09-01`. N08 не начинался.

## N06 native progressive HTTP(S)/FTP(S) (2026-08-31)

- HTTP Ogg/WebM и FTP/FTPS Ogg теперь идут native direct через existing transport/demux registries при `yt_dlp.enabled=false`; Ogg достигает nonzero PCM, WebM — decoded/submitted WGPU frame.
- `service-direct-media` сокращён до capability-driven classification + secret-safe locator; app-owned `direct_progressive_open` владеет HTTP/FTP providers, cancellation, prefetch, demux и progressive lifecycle.
- HTTP 200 body handoff выполняется за один request; Range open/seek/reopen accounting exact 3/3/6; FTP credentials/query redacted, request создаётся через no-HTTP-material constructor, process spy остаётся 0.
- Полные boundaries, tests, commands и limitations: `mem:media-services/native-progressive-http-ftp-n06-2026-08-31`. N07 не начинался.

## G1 native web ingress qualification (2026-08-31)

- N01–N05B foundation прошёл полный gate-only G1: neutral boundaries/config v10/adapter isolation/secret-safe persistence/exact process reasons подтверждены self-review, process-spy parity и всеми workspace/pre-PR/release/Serena gates.
- Coverage baseline квалифицирован exact 9/9 intersection без measurement exceptions и двумя fresh repeatability PASS; authoritative hashes/counts находятся в `mem:testing/coverage`.
- Gate-only root fixes, module locations, dependency-security closure и полный verification handoff: `mem:testing/native-web-ingress-g1-2026-08-31`; suppaftp advisory details: `mem:dependency-security/g1-suppaftp-2026-08-31`.
- Следующая feature session N06 начинается только после отдельного разрешения пользователя.

## N05B provider-neutral same-item/reopen lifecycle (2026-08-31)

- UI, same-item lifecycle, settings, recovery and queue/reopen consumers now use source-owned neutral intent methods and never inspect the concrete ingress provider.
- `ExtractorMediaSourceState` owns candidate/composed tokens and catalog reverse routes inside extractor adapter modules; `WebMediaStreamConfiguration` stores only N01 selections and safe projections.
- The N04 compatibility bridges, temporary stream-model catalog route module and old generic web variants are removed. Active no-op, pending single-flight, Playing/Paused + position/item/lineage preservation and honest failure behavior are covered by functional tests.
- Focused lifecycle/stream/open/settings/suspend/persistence tests, strict app Clippy and workspace all-features check passed. Full boundaries, invariants, audits and G1 exclusions: `mem:app-egui/native-web-ingress-n05b-2026-08-31`.

## N05A provider-neutral web media catalog/sidebar (2026-08-31)

- `WebMediaSelectionTarget` и весь catalog model теперь provider-neutral: N01 `WebMediaSelection` targets для extractor и inert `InstalledOnly` row для direct/native HLS; exact/semantic generation fences и secret-safe Debug сохранены.
- Read-only URL sidebar использует единый safe web projection и provider-neutral `CatalogBacked` model; один installed variant видим, но не создаёт action. Separate A/V projection выбирает максимум одну ranked audio pair на video и не строит Cartesian combinations.
- На момент N05A provider selections были изолированы во временном `web_media_stream_model/catalog_routes.rs`; N05B удалил этот bridge и перенёс reverse routes к extractor adapter owner-у (см. `mem:app-egui/native-web-ingress-n05b-2026-08-31`).
- Focused catalog/stream/sidebar/lifecycle tests, strict app Clippy и workspace all-features check прошли. Full details and N05B removal obligation: `mem:app-egui/native-web-ingress-n05a-2026-08-31`.

## N04 unified app web envelope (2026-08-31)

- `ActiveMediaSource`, `MediaOpenSourceRequest`, and `PreparedMediaDescriptor` now each expose exactly one neutral web variant; local files remain separate.
- Direct, native HLS, and extractor preparation share one app composition boundary preserving metadata, safe label, neutral selection/catalog, exact VOD/live kind, named seek semantics, live timeline, playback window, native initial position, recovery strategy/attachment, and optional extractor reason.
- Durable active source contains only reconstructible root intent/selection; endpoint-bearing recovery material remains descriptor/runtime-only. Controlled reopen preserves neutral selection. N05B удалил временные UI/settings compatibility bridges до G1.
- Local N04 commit; three 110/110 media-open cohorts, focused URL/same-item/stream/startup owners, strict Clippy and workspace all-features check passed. Public-media/GUI/hardware were NOT RUN. Full boundaries and commands: `mem:media-services/native-web-ingress-n04-2026-08-31`.

## N03 typed extractor adapter/process spy (2026-08-31)

- `service-ytdlp::YtDlpExtractorAdapter` carries N01 `ExtractorInvocationReason` through candidate/topology/metadata and every platform-hijack recovery subprocess. All OS child starts cross one instance-injected launcher; production default has no global mutable hook.
- Existing `process_tree::OwnedProcess` still exclusively owns process group, cancellation, timeout, output budgets, cleanup/reap and pipe joins.
- App now has a narrow yt-dlp snapshot projection into the existing neutral planning catalog, N01 selection (including canonical component selection), VOD/live presentation and same-generation metadata; `ActiveMediaSource` migration remains N04.
- Full boundaries, functional fixtures, commands and known S42 missing-evidence-file limitation: `mem:media-services/native-web-ingress-n03-2026-08-31`.

## Stable coverage v2 foundation (2026-08-30)

- Blocking coverage uses three-run source-coordinate stability, not legacy aggregate one-run counters. `coverage/baseline.json` schema v2 and atomic `coverage/measurement-exceptions.json` are the only blocking policy pair; embedded v1 + `coverage/exceptions.json` are frozen report-only provenance. A routine gate is one 3-run cohort; a reviewed baseline update after concurrency-sensitive test changes requires three independent cohort-ов (9 measured runs), exact cross-cohort stable intersection, file-local audit and two fresh post-install repeatability checks. Exact workflow and current hashes live in `mem:testing/coverage`.
- Runtime-built executables are owned by typed policy/prewarm/quarantine and semantic content inventories; CI `Coverage ratchet` compares the previous/current pair through the sole v2 updater before `scripts/coverage.sh check`.
- Full architecture, commands, accepted counts and tests: `mem:testing/coverage`; workflow wiring: `mem:ci/github-actions`.

## HLS VOD manifest-owned worker-receipted seek/cancellation (updated 2026-08-28)

- Native HLS VOD alone opt-in uses typed `PreferPostTargetRap`; yt-dlp HLS VOD keeps default containing-segment decode-forward semantics, and live HLS remains on its separate legacy/live path.
- `Demuxer::seek_with_cancellable_preview_request` plus HLS request-scoped tokens physically cancel superseded preview body reads. Component and separate A/V replacements stage packet-derived anchor/index/diagnostics offside and commit only after one `complete()`; cancellation preserves the old committed source/pair.
- HLS encrypted media/external key full-resource reads use bounded cancellable streaming, so partial ciphertext/key cannot enter packet publication or key cache. InitialOpen/InitialRestore and Preview/FinalReceipt selection markers publish via neutral `log` target only from authorized commit.
- Release GUI acceptance on clean committed HEAD `72a3cbf7` confirmed cold open/restore, 19 final seeks, causal rapid cancellation, actual timeline drags, video/audio/UI progress and strict secret-safe marker parsing. One warm `1169 ms` residual occurred before receipt in external body delivery; post-receipt readiness was `18–19 ms`, so it is not a durable player/CDN latency contract.
- Real x36 manifest had discontinuity sequence 0; cross-epoch correctness is backed by synthetic integration tests. Full boundaries, verification and limitations: `mem:media-services/hls-vod-manifest-receipted-seek-2026-08-24`, `mem:media-services/hls-preview-receipt-cancellation-2026-08-27`, `mem:media-services/hls-manifest-selection-diagnostics-2026-08-27`.

## Hardware AV1 Main/Profile 0 (2026-08-24)

- Native VA-API AV1 is production-ready for Main/Profile 0 YUV420 only: 8-bit NV12 and 10-bit P010 DMA-BUF. The private adapter owns multi-OBU temporal-unit consumption/retry; public neutral APIs were not widened.
- Radeon 780M/Mesa real hardware-only and full playback matrices passed through configured AV1 adapter, exact DMA-BUF format and renderer submit. Exact boundaries, rejects, tests and commands: `mem:video-vaapi/av1-hardware-2026-08-24`.

## AUD-020 abortable superseded manifest fetch (2026-08-24)

- Independent loopback verification confirmed dormant public `AdaptiveManifestFetcher` serialized B behind hanging A: 300 ms hold, B started at 303 ms, A stayed connected; stale publication fence itself worked.
- `source-core` now owns a lazy async session frontend and runtime-hidden one-future `AbortableHttpTaskExecutor`; `web-media-adaptive` remains Tokio-free and only owns generation/retry/publication semantics.
- Supersede/source-cancel/Drop physically disconnect current TCP work; rapid A -> B -> C publishes only C. source-core 66/66, adaptive 44/44, strict Clippy, primary/MSRV workspace checks and guardrails passed.
- Full boundaries, regression properties and dormant production reachability: `mem:media-services/manifest-supersede-cancellation-aud020-2026-08-24`.

## AUD-019 bounded next-item source/demux preload (2026-08-24)

- Independent verification confirmed every clean EOF performed a cold strong-open; option A now prepares only the exact next source/demux, while decoder/backend/auth/packets/current identity remain unchanged until EOF.
- `PlaylistRuntime::PreparedNextOwner` owns exact identity+queue-revision+item correlation, one physical speculative worker, 64 MiB aggregate RAM/read-ahead default budget, 30 s lead and 120 s hold; disable/settings/mutation/suspend/authoritative open/shutdown cancel authority.
- Ready exact envelopes enter the unchanged strong protocol at EOF; preparing/failed/stale/mismatched/expired states preserve the cold-open fallback. Config schema v9; config 93/93, app 970/970 and strict Clippy passed.
- Full boundaries, transition matrix and commands: `mem:app-egui/next-item-preload-aud019-2026-08-24`.

## AUD-018 truthful playback-smoke dry-run outcomes (2026-08-24)

- Independent verification confirmed direct probe-only and legacy-migration dry-runs exited 0 and emitted production PASS without executing Cargo; runner markers are written to stderr.
- Shared `report_acceptance_outcome()` now emits only `DRY-RUN: WOULD RUN ...; no checks were executed` for dry-run and preserves PASS only after successful real steps under `set -e`.
- Public-CLI self-tests cover direct/full dry-run plus fake-Cargo success and exit-17 failure orchestration; real legacy migration passes 9/9. Full boundary, commands and headless probe limitation: `mem:testing/playback-smoke-aud018-2026-08-24`.

## AUD-017 bounded HTTP Retry-After (2026-08-24)

- Independent production-boundary verification confirmed `429 Retry-After: 2` and standard HTTP-date were discarded and retried after local ~5.6 ms; malformed safely used the same fallback.
- `source-core` now projects raw headers into secret-safe typed `HttpRetryAfter`; adaptive policy independently caps server hints, and blocking/manifest/segment retry paths preserve cancellation and bounded deadlines.
- Post-fix harness: delta 2 → 2000.792 ms, HTTP-date → 2667.244 ms, malformed → 5.876 ms. `source-core` 60/60, adaptive 40/40, primary/MSRV checks and strict affected Clippy passed.
- Full boundary, tests, commands and policy cap: `mem:media-services/http-retry-after-aud017-2026-08-24`.

## AUD-016 DMA-BUF frame contract до unsafe import (2026-08-23)

- Independent production-boundary verification confirmed topology-valid wrong coded dimensions and `ComposedLayers`/`SeparateLayers` mismatch reached the DMA-BUF importer.
- `video-core` now owns a typed full DMA-BUF/frame-contract validator; WGPU materializer rejects mismatches before cache/importer, while the existing `VideoFrameLease` remains the exactly-once release owner.
- Functional fake-provider/recording-importer regressions prove mismatch importer calls 0, valid calls 1 and exactly-once release. Full boundary, tests and limitation: `mem:render-video/dma-buf-frame-contract-aud016-2026-08-23`.

## AUD-015 bounded FFmpeg worker shutdown/join (2026-08-23)

- Independent production-worker verification confirmed full host pool + queued packet + dropped frontend/control + held resource never terminated: 250 ms and 8 s both timed out; disconnected control receiver caused immediate re-selection/busy-spin.
- `FfmpegVideoDecoderThread` now owns a separate shutdown signal and `JoinHandle` through `FfmpegWorkerLifecycle`; Drop signals independently of pool/packet pressure and joins exactly once, while control disconnect is terminal.
- Production-frontend regression proves bounded `drop + join`; `video-ffmpeg` 88/88, no-feature 60/60, `player-core` 646/646 and strict Clippy passed.
- Full boundary, regression and limitation: `mem:video-ffmpeg/bounded-worker-shutdown-aud015-2026-08-23`.

## AUD-014 bounded seek settlement перед lifecycle checkpoint (2026-08-23)

- Independently reproduced: pending accepted 90 s seek плюс immediate suspend restored stale 10 s snapshot.
- App transport теперь ждёт exact seek receipts до общего 1 s deadline; Applied сохраняет settled target, timeout/missing owner сохраняет typed documented pre-seek position.
- Suspend/restore и shutdown sidecar проходят production-boundary regressions; full boundary, tests и invariant: `mem:app-egui/timeline-seek-lifecycle-settlement-aud014-2026-08-23`.

## AUD-013 vertical seek acceptance до renderer submit (2026-08-23)

- Independent read-only verification confirmed no real compressed asset reached production demux → decoder → materializer → WGPU video draw/submit/completion release before and after nonzero seek.
- A blocking CI row now generates H.264/MPEG-TS locally and proves one FFmpeg software + HostPlanar WGPU vertical on the same demux/decoder: generation 1 / PTS 0, flush + seek 2 s, generation 2 / PTS 2 s, non-black readback and submitted release.
- Remaining Smooth and VA-API rows stay explicit NOT COVERED/NOT RUN. Full boundary, command and limitations: `mem:testing/vertical-seek-acceptance-aud013-2026-08-23`.

## AUD-011/AUD-012 queue continuation и exact seek identity (2026-08-23)

- Независимая production-boundary проверка подтвердила оба P1: sync pre-request failure терял automatic plan и входил в manual D55, а delayed bare BeyondEnd(A) после Installed B создавал Next(C).
- Unstaged failure теперь потребляет exact `PlannedPlaylistInstall`, общий automatic tail сохраняет opaque traversal/budget/loop guard, а app продолжает sync failures bounded loop-ом; удалённая B всё равно даёт stageable C.
- Все `ExactTimelineSeekOutcome` и receipt несут `MediaInstanceId`; runtime-owned stale fence не позволяет A двигать active B, а receipt batch остаётся identity-bearing и coalesce-ит только повтор matching instance.
- Full boundary, regressions, commands и GUI limitation: `mem:app-egui/queue-seek-identity-aud011-aud012-2026-08-23`.

## AUD-010 bounded whole-timeline HLS VOD seek index (2026-08-23)

- Independent production-path verification confirmed the four-entry muxed A/V index froze at 30 s after six segments; seek 150 s refetched five segments instead of one.
- `HlsSeekIndex` now owns fair video/audio whole-timeline compaction, preserves early/fresh coverage when budget permits, reuses unused kind capacity and keeps preview pins exact; live/DVR remains on its separate sliding evidence owner.
- Runtime regression seeks 155 s with limit 4, selects RAP 150 s and fetches only segment 5; downstream player contracts reach decoder and presentation. Full boundaries and verification: `mem:media-services/hls-vod-seek-index-compaction-aud010-2026-08-23`.

## AUD-009 bounded VOD endpoint recovery (2026-08-23)

- Independent read-only verification confirmed progressive, HLS VOD, DASH VOD, Smooth and HDS remained terminal after signed endpoint expiry; live HLS/DASH refresh did not cover these VOD lifecycles.
- Provider-neutral typed expiry now reaches an armed candidate gate and app-owned exact Installed binding; old demux publication is held while whole-candidate semantic yt-dlp re-extraction uses same-lineage staged install and preserves late-seek target. Speculative probe failures are ignored until candidate finalization.
- Config schema v8 owns bounded attempts/backoff/stable reset. Full boundaries, regressions and verification: `mem:media-services/vod-endpoint-recovery-aud009-2026-08-23`.

## AUD-008 row-local yt-dlp planning rejections (2026-08-23)

- Independent production-path verification confirmed fail-fast planning discarded a valid H.264 row when a neighboring normalized bare HEVC row could not produce a runtime requirement.
- Service-owned `YtDlpPlanningProjection` now separates a neutral plannable snapshot from exact typed per-row rejections; production app diagnostics count planning rejections, while source/generation/duplicate identity remain fatal snapshot invariants.
- Functional regression and full verification: `mem:media-services/ytdlp-row-local-planning-rejections-aud008-2026-08-23`.

## AUD-007 bounded yt-dlp single-item output/DOM (2026-08-23)

- Independent verification confirmed unbounded stdout/stderr and full JSON DOM caused proportional RSS growth up to ~1007.7 MiB for 500+500 MiB output.
- New process-output owner enforces configurable stdout 64 MiB, stderr 8 MiB and JSON 1,000,000-value defaults chosen from real `yt-dlp 2026.08.19` profiling with ~81x/~84x reserve; typed overflow terminates/waits the owned process group.
- Compact-node 8 MiB JSON dropped from ~267.5 MiB to ~11.6 MiB RSS before DOM; valid 32 MiB JSON still reaches an accepted candidate. Full boundaries, tests and headless-process RSS limitation: `mem:media-services/ytdlp-output-budgets-aud007-2026-08-23`.

## AUD-006 development-only system yt-dlp compatibility check (2026-08-23)

- Production config/cookies/plugins и version-independent runtime behavior намеренно сохранены; version allowlist/preflight не добавлены.
- `scripts/ytdlp-compatibility.sh` через временный hermetic shim и loopback fixture прогоняет фактический system executable через public candidate/topology production boundaries; `/usr/bin/yt-dlp 2026.08.19` — PASS.
- Ignored real-system test, shell self-test, workflow и принятое ограничение описаны в `mem:media-services/ytdlp-system-compatibility-aud006-2026-08-23`.

## AUD-005 durable FFmpeg packet completion accounting (2026-08-23)

- Real production MPEG-TS accurate-seek/EOF stress confirmed bounded ACK loss: capacity 1, accepted 16, delivered 1, false terminal in-flight 15 after actual decoder `Drained`; 5/5 repeats.
- FFmpeg worker completion truth now lives in a durable atomic accumulator; activity remains only a coalesced wake hint. Post-fix real regression: accepted/completions 16/16, in-flight 0, repeat drain 0, EOF `Drained`; `video-ffmpeg` 87/87, `player-core` 643/643 and strict Clippy passed.
- Full boundary, regression fixture/command and limitation: `mem:video-ffmpeg/durable-packet-completion-aud005-2026-08-23`.

## AUD-004 decoded-frame batch tail release fix (2026-08-23)

- Fatal frame-contract mismatch в `player-core` теперь exactly-once освобождает текущий handle и весь уже извлечённый хвост decoder receive-batch через прежнюю release boundary; API/error/accounting semantics не менялись.
- Fake decoder regression с handles `81/82/83` закрепляет mismatch на первом и втором frame, затем decoder replacement и равенство accepted/released sets. `player-core` 643/643 и strict Clippy прошли.
- Полный ownership invariant, test anchors и команды: `mem:player-core/decoded-frame-contract-mismatch-tail-release-aud004-2026-08-23`.

## AUD-003 PTS-only software FFmpeg time-base fix (2026-08-23)

- Real generated MPEG-TS H.264 no-B-frame verification confirmed that player/decoder protocol dropped raw `track_pts` when `track_dts=None`, producing repeated materialized frame PTS at start and after seek.
- `track_pts` now crosses player-core and neutral decoder protocol; FFmpeg sets exact packet timestamps plus `AVPacket.time_base` and `AVCodecContext.pkt_timebase`. Real start PTS are `0/200000/400000 us`; seek PTS are `2000000/2200000/2400000 us` for a 2 s target, with current generation and terminal EOF drain.
- Full boundaries, focused/real tests, command and GUI/WGPU limitation: `mem:video-ffmpeg/pts-only-packet-timebase-aud003-2026-08-23`.

## AUD-002 dependency/security gate closure (2026-08-23)

- Blocking RUSTSEC-2026-0221 и RUSTSEC-2026-0257 устранены точечным lock update: `event-listener 5.4.2`, `webbrowser 1.2.2`; manifests и desktop feature boundaries не менялись.
- Dependency gate, locked feature trees, desktop-integration 25/25, app-egui 950/950, primary workspace check и MSRV 1.92 workspace check прошли. Реальный system-browser launch остаётся manual smoke.
- Полные dependency chains, lock scope, policy и команды: `mem:dependency-security/aud-002-2026-08-23`.

## AUD-001 post-Installed strong-open compensation (2026-08-23)

- Generic stepwise и blocking strong-open больше не теряют cleanup obligation после exact `Installed`: restore/intent/app-registration failure запускает exact player release, ждёт matching `Applied`, затем owner-owned controller/source reconciliation и только после этого публикует исходную ошибку.
- Same-lineage rebind перенесён за успешный playback-intent; cleanup failure остаётся typed fatal и не разрешает navigation recovery. Controller functional regression доказывает A -> failed Installed B -> release -> exact Installed C.
- Полные boundaries, tests и AUD-013 limitation: `mem:app-egui/post-installed-strong-open-compensation-2026-08-23`.

## S42 scoped final acceptance (2026-07-25)

- `crates/service-ytdlp/compatibility/2026.07.04/final-acceptance-s42.json` owns scoped `ProfileTraceabilityComplete`: all 12 canonical `Implemented` rows bind exact provider/demux/decoder/runtime-fixture/capability evidence, aggregate RTMP and extended absences remain typed exclusions/no-op, recursive S00/S41/S42 `Planned` inventory is empty, and `traceability_gaps` is empty.
- `crates/service-ytdlp/compatibility/2026.07.04/roadmap-trace-s42.json` separately owns the complete machine-readable §14/release trace: exactly 31 hermetic rows plus 15 mandatory audits and one manual-non-automation audit, backed by 117 checked-in Rust/Python/shell evidence entries. `final_acceptance_s42::roadmap_trace` exact-ratchets IDs, schemas, paths, symbols, Cargo/Python/shell targets and dead references.
- Cross-cutting executable evidence is exact and source-validated for auth/secret non-leakage, acknowledged locator vs structurally unrepresentable transient request material, cancellation/stale generation, bounded shutdown, and real player-owned pre-barrier import/open/switch preservation. The validator is `cargo +1.96.0 test -p service-ytdlp --test final_acceptance_s42 --locked`.
- `scripts/final-acceptance.sh` completed `S42 automated acceptance: PASS` on primary Rust 1.96.0 with locked MSRV 1.92. Manual 29-case URL/fixture acceptance remains `NOT RUN`; real VA-API rerun is also `NOT RUN` because the owner has no compatible device. The only accepted hardware delta remains exact `VAProfileH264Baseline` → H.264 Baseline 8-bit YUV420/NV12, capability intersection only.
- App admission now rejects exact RTMP/RTMPE before provider lookup as `StartupUrlUnsupportedReason::ProfileExcludedInputScheme`; injected capabilities cannot bypass the profile. Pure locator parsing still preserves their exact typed identity. RTMP wire, RTSP/RTP/MMS, private-live state and DRM remain excluded.
- S42 public DASH-live evidence exposed two shared-state self-deadlocks in `prepare_dash_live`; initial plan and post-open authoritative live edge are now copied under separate short guards before re-entrant source open/seek. The second read must remain after open because synchronous endpoint recovery may replace the snapshot. See `mem:media-services/dash-live-s35-2026-07-24`.
- Coverage inventory is fail-closed at 47 blocking and 11 informational crates with cargo-llvm-cov 0.8.7. Conservative measured baseline: workspace lines 135834/181804, functions 13197/17245, regions 169757/228313; blocking group lines 83276/99646, functions 8338/10114, regions 103632/125867. Exactly 28 owner-approved S42 exception rows remain; no row was added for scheduler stabilization.
- Cargo-deny leaves only two explicitly nonblocking unmaintained advisories with no safe upgrade: RUSTSEC-2026-0150 (`audiopus_sys`) and RUSTSEC-2026-0192 (`ttf-parser`). XML advisory graph is clean.

## S41 cross-provider integration (2026-07-25)

- 12 exact S00 runtime rows now have checked-in `Implemented` coverage; aggregate RTMP stays explicit S39 `ProfileExcluded`, while S36L/S38L/S40P expansions remain no-op/excluded rather than fake providers.
- Normal open, startup and settings rebuild now converge through one app-owned named `PreparedMedia` attachment boundary before the unchanged strong Ready -> authorize -> Installed protocol. Full manifest, ownership and verification: `mem:media-services/cross-provider-integration-s41-2026-07-25`.

## S40 serializable special-provider gate (2026-07-25)

- S40 завершён как доказанный no-op: S00 не содержит отдельной `PublicSerializable` special-provider target row, поэтому `S40P-*` cards и дополнительные S41 dependencies не созданы.
- `bunnycdn`/`soopvod`/`niconico_live`/`fc2_live`/`websocket_frag` остаются exact typed exclusions; JSON-сериализуемость `protocol` не является provider admission. Production API/dependencies и Python helper/IPC не менялись. Full evidence: `mem:media-services/serializable-special-provider-s40-2026-07-25`.

## S39 exact RTMP family gate (2026-07-25)

- S39 завершён как доказанный no-op: aggregate `rtmp-family-flv` остаётся только identity-only S00 inventory, а exact `rtmp`/`rtmpe` не имеют deterministic wire/crypto fixtures и остаются provisional exclusions. `rtmp_ffmpeg` — жёсткий non-wire exclusion; TLS/tunnel variants не alias-нормализуются.
- Production RTMP provider/dependency/S15A/S21T/S31L changes отсутствуют; current app admission rejects exact RTMP/RTMPE before capability lookup as typed `ProfileExcludedInputScheme`. Full evidence: `mem:media-services/rtmp-family-s39-2026-07-25`.

## S36 Smooth Streaming static VOD (2026-07-25)

- Production web-media open now supports the single approved S00 ISM/MSS row: strict muxed fMP4 H.264 + AAC-LC static VOD with bounded S04X manifest parsing, quality catalog, URL templates/timeline repeats, F1/F2 reconstruction, exact audio presentation windows, stable A/V composition and transactional receipted seek.
- `web-media-smooth` owns provider-neutral preparation/sources/demux orchestration; app injects the existing S28A/F3A Symphonia registry and publishes the fresh C3 selection only through the normal Ready → authorize → Installed barrier. ISM live/DVR and other codecs remain excluded and no S36L card exists. Full contract: `mem:media-services/smooth-vod-runtime-s36p4-p6-2026-07-25`.

## S35S neutral live same-item candidate switch (2026-07-24)

- Proven S25 same-lineage switching now restores an old absolute position only when it remains inside the latest fresh-generation DVR range; expired/no-DVR switches keep the provider-declared safe live edge and return typed `AdjustedToLiveEdge`.
- The decision is player-owned over the installed `DynamicMediaTimelinePort`; app keeps exact Installed/rebind ordering and live checkpoints remain non-persistent. Full boundary, tests and post-install seek-expiry limitation: `mem:app-egui/live-same-item-candidate-switch-s35s-2026-07-24`.

## S35 strict DASH live/DVR (2026-07-24)

- Production dynamic DASH now supports the deliberately narrow checked-in timing profile: direct UTC midpoint synchronization, SegmentTemplate+SegmentTimeline with PTO, strict Period timing, A/V availability intersection, publish-ordered refresh, sliding DVR and endpoint re-extraction through existing S31L/player boundaries.
- Unknown/ambiguous timing, timeline gaps/overlaps/boundary-crossing segments, `availabilityTimeComplete=false` and all partial/chunked LL-DASH semantics fail closed as typed profile exclusions. Full ownership, defaults, fences, tests and limitations: `mem:media-services/dash-live-s35-2026-07-24`.

## S34 static DASH VOD (2026-07-24)

- Production DASH now supports exact static MPD and serialized `http_dash_segments` inputs, proven fMP4/WebM muxed/single/separate layouts, bounded Periods and nonblocking generation-fenced receipted seek through the existing install barrier.
- Pure MPD ownership is `dash-mpd-core`; runtime orchestration is `web-media-dash`; `service-ytdlp` remains request-material only; app owns composition; player sees only a neutral prepared-demux seek port. Full contract: `mem:media-services/dash-vod-s34-2026-07-24`.

## S33 HLS live/DVR (2026-07-24)

- Explicit yt-dlp live intent now opens a provider-owned HLS live runtime with independent rendition refresh, segment-scoped TS/fMP4 provenance, proven sliding DVR through the existing S31L port, durationless `TracksChanged`, ENDLIST drain and typed LL-HLS exclusion.
- Expired manifest/segment/init/key endpoints use secret-safe single-flight app-owned re-extraction + semantic rematch + atomic fresh transport generation; player-core API and queue/barrier ownership are unchanged. Full contract: `mem:media-services/hls-live-s33-2026-07-24`.

## S32B HLS VOD runtime before seek/app integration (2026-07-23)

- `web-media-hls` now prepares uninstalled master/media VOD runtime with strict TS/fMP4 evidence, MAP/ranges, alternate audio, AES and discontinuity epochs over S31 fetch + injected neutral demux registry. Runtime is intentionally NotSeekable and app-unwired until S32C. Full handoff: `mem:media-services/hls-vod-s32b-2026-07-23`.

## S32A HLS VOD parser/request/AES foundation (2026-07-23)

- Shared `hls-playlist-core` now owns bounded RFC 8216 master/media parsing and initial-profile rejection; `playlist-io` reuses it for classification. `web-media-hls` owns audited AES-128/key state, while service-ytdlp publishes exact inline/query/hls_aes intent and source-core owns query merge. Network/demux is S32B and decode-safe seek/app integration is S32C. Full handoff: `mem:media-services/hls-vod-s32a-2026-07-23`.

## S31L neutral dynamic live/DVR timeline (2026-07-23)

- Provider-neutral live timeline contract now lives in `media-core`; player owns installed generation/revision projection and worker wait integration, app owns UI/desktop wake projection.
- Live is durationless, optional-DVR, never silently clamps expired targets, and never persists a resume checkpoint.
- Static CUE playback windows and live timeline mode are a typed mutually-exclusive `PreparedMedia` intent.
- Full contract and test locations: `mem:player-core/dynamic-live-timeline-s31l-2026-07-23`.

## S31 adaptive transport foundation (2026-07-23)

- Новый `web-media-adaptive` владеет bounded manifest/segment fetch, retry/backoff/cancel, generation/refresh fencing, neutral VOD/live/DVR + per-component clock metadata и explicit nonblocking segment readiness. `source-core` остаётся единственным HTTP owner; S21T secret/redirect policy переиспользована. Existing finite `OrderedSegmentSource` не ломался; deferred demux open выполняет initial readiness/sniff/parser на worker-е и публикует S21R `TemporarilyUnavailable` player-owner-у. Полный handoff: `mem:media-services/adaptive-transport-s31-2026-07-23`.

## S30 FLV/F4F demux (2026-07-23)

- Новый first-party `flv-demux` реализует bounded raw FLV и strict F4F ordered-segment adapter с selected legacy/enhanced codec mappings, config/keyframe lifecycle, AMF0 index, transactional seek и recovery.
- App web demux composition S30 агрегирует Symphonia + FLV/F4F из exact descriptor rows; accidental MPEG-TS web registration/hint отсутствует (существующий local S29 path не менялся). `f4v` исправлен на ISO-BMFF, `f4f` остаётся только OrderedSegments. Полный handoff: `mem:flv-demux/core`.

## S29 MPEG-TS demux + local playback (2026-07-22)

- Новый first-party crate `mpeg-ts-demux` владеет reusable 188-byte MPEG-TS path: bounded sync/resync, PAT/PMT и fail-closed multi-program selection, continuity/PES, independent PTS/DTS wrap, PCR evidence, H.264/H.265 Annex-B AU assembly across PES, AAC/ADTS и header-proven MP1/2/3, config/keyframe lifecycle, typed discontinuity/`TracksChanged`, streaming/ordered inputs и capped sparse VOD index с bounded on-demand expansion. 192-byte M2TS, private/LATM/AC-3 stream types, HLS и network policy остаются вне S29.
- Локальные файлы теперь открываются через app-owned `DemuxRegistry` с Symphonia + MPEG-TS factories над тем же `LocalFileSource`; signature сильнее extension, `.ts` добавлен в picker hints, cancellation/fingerprint/revalidation остаются typed и one-handle.
- Neutral `media_core::VideoPacketFraming` отделяет Annex-B от codec-configuration-derived length-prefixed packets; player-core больше не требует fake hvcC для H.265 TS. Полный handoff: `mem:mpeg-ts-demux/core`.

# Core

## Slice G unified web-media picker (2026-07-26)

- The web-media picker publishes a complete planner-playable yt-dlp declared catalog synchronously after exact Installed: no sibling candidate open, top-N limit, catalog worker or provider-manifest enrichment. Dependent mode/codec/resolution/FPS/HDR UI, session-only per-item semantic preferences, exact-Installed automatic restore/fallback and planner-owned source-order-independent opaque grouping remain active; selected-candidate HLS/DASH/HDS/Smooth component axes stay separate. Full ownership, fencing and verification are in `mem:app-egui/web-media-picker-slice-g-2026-07-26`.

## S28G existing-demux hardening gate (2026-07-22)

- S28A/B/C сведены в один reuse foundation без новой runtime feature logic и без изменения demux API/seek/event semantics. Exact Matroska `DocType`, fMP4 identity, полный fixture inventory, blocking coverage classification и parser-ownership guardrails закреплены hermetic evidence.
- Matroska packet/container parsing остаётся только в exact `symphonia-format-mkv` patch; `matroska_metadata.rs` — осознанный bounded fail-open индексатор `Tracks`/`Colour`/`SeekHead`/`Cues`, которому запрещено разрастаться до Cluster/Block/lacing/packet parsing. Полный handoff: `mem:symphonia-demux/existing-demux-s28g-2026-07-22`.

## S28C current audio-container proof (2026-07-22)

- Existing Ogg/Opus, CAF/PCM, WAVE/PCM, AIFF/PCM, native FLAC and distinct MP1/MP2/MP3 paths are now proven hermetically across local, progressive Range/non-Range, S20/S21C capability intersection, codec-private/packet timing, duration/seek, malformed/cancel/sniff and Ogg reset lifecycle.
- User-selected full progressive CAF support is implemented by an inventoried exact `symphonia-format-caf 0.6.0` replacement. Stable boundaries and limitation: `mem:symphonia-demux/audio-containers-s28c-2026-07-22`; patch maintenance: `mem:dependency-patches/core`.


## S24 URL sidebar stream model (2026-07-22)

- Existing URL sidebar теперь показывает только active web-media configuration: safe source label, S21C-playable resolutions/formats, active/pending projection, global-vs-item preference, group-part scope, VOD/seek/buffering/refresh и bounded safe failure. Local source не активирует web model; direct-media не получает fake choices.
- Safe inventory проходит тот же S19→S21C→S23 preparation и публикуется только с exact Installed `ActiveMediaSource`; secrets/raw identities отсутствуют. Единственный sidebar Panel/geometry owner сохранён. Полный boundary и S25 limitation: `mem:app-egui/sidebar-controller`.

## S23 queue-owned web open (2026-07-22)

- Current yt-dlp playback no longer uses the historical WebM-only service opener. `app-egui::web_media_open` composes S19 candidates -> S21C planner -> S22 transport/demux; `PlaylistRuntime` retains exact Item/revision/barrier ownership and publishes current/active only after exact Installed. S26 maps effective system yt-dlp headers/cookies into scoped ephemeral transport state with per-source Set-Cookie handling and zero persistence; full boundary: `mem:media-services/ytdlp-system-auth-s26-2026-07-22`.
- `service-ytdlp` owns extractor/topology/locator/metadata plus neutral planning/transport mapping, with no concrete HTTP/demux/player dependencies. Exact rebuild stores `YtDlpCandidateSelection` and performs fresh-generation semantic rematch.
- Full boundaries, cancellation, S26 auth limitation and verification: `mem:app-egui/queue-owned-web-open-s23-2026-07-22`.


## S17 topology-first Add URL (2026-07-20)

- Toolbar Add URL сохраняет direct-media-first routing: direct URL остаётся single append, yt-dlp URL теперь идёт через process-lifetime latest-only topology worker -> S16 ID-less mapping -> единственную S08 AppendToQueue preview/confirmation/commit transaction. Rapid submit/cancel/shutdown используют exact generation fencing; collection/multi_video, unavailable, metadata и whole-group capacity сохраняются без current/playback mutation. Полный контракт: `mem:app-egui/url-collection-import-s17-2026-07-20`.

## Актуальный generic yt-dlp/config v6 update (2026-07-17)

- S00 (2026-07-20) добавил только checked-in compatibility evidence для official `yt-dlp 2026.07.04`, не runtime feature: canonical owner `crates/service-ytdlp/compatibility/2026.07.04/`, manifest `profile.json`, report/capture procedure/synthetic corpus и focused test `crates/service-ytdlp/tests/compatibility_profile.rs`. Manifest отдельно фиксирует будущий hermetic argv (`--ignore-config --no-plugin-dirs ... --simulate --dump-single-json`) и current manual opt-in argv; production process пока продолжает читать system/user config/plugins, их side effects и mutable user cookie jar остаются вне app guarantee. Полный контракт: `mem:media-services/core`.

- Старый YouTube-only crate переименован в `service-ytdlp`; public/internal vocabulary — `YtDlp*`. Он является generic fallback для любого remaining absolute HTTP(S) URL после приоритетного `service-direct-media` adapter-а. `player-core`, decoder и renderer не изменены и получают только готовый `PreparedMedia`.
- `YtDlpMediaLocator` хранит exact secret identity без query normalization; safe formatting скрывает userinfo/path/query/fragment. Current playback admission идёт через S19 normalized muxed/separate candidates, S21C capabilities/policy и S22 progressive HTTP(S) transport; HLS/RTMP/fragments и S26-owned headers/cookies до соответствующих stages fail-closed до commit barrier. Подробности: `mem:app-egui/queue-owned-web-open-s23-2026-07-22`, `mem:media-services/secret-safe-locators-s10b`.
- App composition/reopen/suspend/settings/metadata variants называются `YtDlp`; metadata enrichment generic и остаётся side-effect-free относительно playback. См. `mem:app-egui/media-open-coordinator-s10c` и `mem:app-egui/ytdlp-playlist-metadata-2026-07-17`.
- Config current schema — v7: legacy v2-v5 `[youtube]` мигрируется в `[yt_dlp]`, v6 получает default `preferred_video_height = None`, placeholder `prefer_account_session` удалён. Global `yt_dlp.preferred_video_height: Option<PreferredVideoHeight>` config-owned и app maps его в neutral web-media policy; exact→lower→higher действует после HDR/codec buckets. См. `mem:config/schema-v7-quality-preference-2026-07-21`.

- Rust desktop video player workspace `rustiplayer`; Serena memories are the primary project knowledge source. Start with this `mem:core` entry, then follow the focused memory references below.
- Read these focused memories when needed: stack/deps in `mem:tech_stack`, local commands in `mem:suggested_commands`, code/architecture rules in `mem:conventions`, completion checks in `mem:task_completion`, dependency patch policy in `mem:dependency-patches/core`.
- Module maps: playback/session boundaries in `mem:player-core/core`, concrete audio output/clock invariants in `mem:audio/core`, media/services/source flow in `mem:media-services/core`, direct HTTP media opener policy in `mem:media-services/direct-media`, render/video backend flow in `mem:render-video/core`.
- Workspace split: `animation-core` neutral UI animation math (easing + `SlideTransition` + safe `visibility::{VisibilityEffect, VisibilitySample}`, no egui/wgpu/clock deps; used for settings sidebar slide-in and reusable fade/fade-scale UI transitions, details in `mem:settings-ui/design` and `mem:animation-core/visibility-2026-07-18`); `video-frame-contract` neutral decoder->renderer frame/transfer vocabulary (`VideoFramePixelLayout`, `VideoFrameContract`, `VideoFrameTransferPath`, `HardwareFrameHandle`, `DmaBufImageLayout`) with serde-on public schema types and no deps on codec/video/render/backend crates; `video-present-core` neutral RAII present-frame lease/drop/release vocabulary (`VideoFrameLease`, `VideoFrameRelease`, `VideoFrameReleaseSink`, typed lookup descriptor/sample types) shared by playback/render and future frame server; its present resource kind keeps DMA-BUF zero-copy and HostPlanar host-upload distinct while staying free of `player-core` and concrete renderer/backend deps; `frame-server-core` is the neutral frame-server/scrub protocol contract crate (details in `mem:frame-server/core`); `natural-sort-key` is the std-only compact prepared natural filename comparator shared by playlist domain and filesystem discovery while each owner retains exact path/locator tie-breakers; `playlist-core` is the neutral canonical playlist domain owner for stable Item IDs, monotonic allocator/restore, reversible locators, D08 reservation token, metadata patches, and canonical queue policy (details in `mem:playlist/core`); `playlist-discovery` is the UI/player/config-neutral owner of single-local-file probe metadata/fingerprint/cancellation, bounded deterministic directory manifests, the shared bounded discovery executor/jobs, and D43/D74 admission/readiness result streams without queue/Item-ID/app commit ownership (details in `mem:playlist/discovery`); `app-egui` app composition root; `player-core` worker/session/scheduler; `audio-core` neutral audio decoder/output/clock/tempo contracts; `media-core`/`codec-core`/`video-core`/`video-backend-api`/`video-present-core`/`render-core`/`capability-core` contract crates; `settings-core` project-agnostic settings metadata/registry/accessor/diff/controller transaction contracts with no UI/GPU/project config deps; `settings-derive` proc-macro generated settings registry/accessors from schema metadata, including strict coverage and nested config composition through `settings-core::SettingsSchema`; `source-core` byte access; `media-prefetch` owns neutral prefetch config, pure sliding RAM buffer, debug diagnostics, and the fallible `PrefetchingByteSource`/background worker boundary over `source-core::ByteSource` (`new` returns typed `PrefetchStartupError`; constructed source owns cancellation and join); `service-direct-media` opens generic seekable `http(s)` `.mp4`/`.mkv`/`.webm` media via `HttpRangeSource` -> `PrefetchingByteSource` -> `SymphoniaDemuxer` and returns neutral results without `player-core`; `service-ytdlp` owns yt-dlp extraction/topology/locator/metadata and neutral S19 -> S21C/S21T adapters, while `app-egui::web_media_open` maps committed network/demux config into the concrete S22 transport/demux runtime; `symphonia-demux` concrete demux; `service-ytdlp` yt-dlp adapter; `video-vaapi` VA-API decode; `video-ffmpeg` optional FFmpeg software decoder scaffold with all raw FFmpeg FFI isolated behind feature `ffmpeg`; `render-wgpu-video` pure WGPU video renderer/materializer boundary; `render-wgpu-shell` WGPU window/surface/egui composition shell; `audio` concrete Symphonia/Opus decoder and CPAL output backend; `audio-signalsmith` is the runtime Signalsmith Stretch adapter wired by `app-egui` behind the neutral `audio-core` tempo boundary; `audio-timestretch` remains a workspace probe/evaluation host rather than the runtime backend; `desktop-integration` desktop controls adapter.
- Primary hardware video path remains zero-copy: VP9 Profile 0 SDR -> VA-API -> `VideoFramePixelLayout::Nv12` + DMA-BUF zero-copy -> WGPU SDR; VP9 Profile 2 HDR 10-bit -> VA-API -> `VideoFramePixelLayout::P010` + DMA-BUF zero-copy -> WGPU BT.2446-C HDR-to-SDR. Optional software path is FFmpeg -> AVFrame-backed HostPlanar -> one host-to-GPU upload -> WGPU HostPlanar YUV render. `VideoFrameTransferPath`/`VideoFrameContract` are the active decoder->renderer contract vocabulary; `VideoStreamDecodeConfig.frame_contract` comes from the selected capability output, and `DecodedFrame.frame_contract` must match it.
- FFmpeg/libav software decode is now wired into app composition as an optional playback path when the `video-ffmpeg` runtime probe produces renderer-intersected playable outputs; CPU readback fallback and native HDR output are still not implemented. Since 2026-06-16 (Session 23) `app-egui` enables FFmpeg by default (`default = ["ffmpeg"]` -> `video-ffmpeg/ffmpeg`), so default `cargo build/check/test` of the workspace or the `rustiplayer` binary (debug and release) now compile the FFmpeg software path and require FFmpeg dev libs/runtime; opt out with `--no-default-features` on `app-egui`. The isolation boundary is unchanged: `ffmpeg`/`ffmpeg-sys-next` remain optional deps inside `video-ffmpeg` only, `app-egui` adds no direct FFmpeg dependency, `video-ffmpeg`'s own default feature set stays empty, and `player-core` still does not depend on concrete FFmpeg/render crates. `video-ffmpeg` exposes `FfmpegSoftwareVideoBackendFactory` for neutral backend startup and `FfmpegSoftwareCapabilityProvider` for capability scanning; the provider declares raw `ffmpeg-sw` software outputs only when runtime probe succeeds, and scanner renderer-intersection decides playability. `app-egui` registers both VA-API and FFmpeg providers, selects concrete plans `VaapiDmaBufWgpu` or `FfmpegHostUploadWgpu`, and starts either a VA-API DMA-BUF materializer or an FFmpeg HostPlanar upload materializer before handing only neutral `StartedVideoBackend` handles to `PlayerWorker`. The WGPU renderer has a renderer-side HostPlanar YUV software host-upload render path for the v1 software matrix: `Yuv420Planar8/10Le/12Le`, `Yuv422Planar8/10Le/12Le`, and `Yuv444Planar8/10Le` with `SoftwareHostUpload` contracts. 8-bit uses `R8Unorm` Y/U/V planes, 10/12-bit uses `R16Uint` plane words, and GPU shaders do YUV sampling/color conversion/HDR-to-SDR where the high-bit shader contract supports it. These host-upload contracts are advertised by `RenderCapabilities`; public config remains `auto`/`hardware`/`software`, where `auto` prefers playable VA-API hardware and only then FFmpeg software, `hardware` never falls back to software, and `software` never starts VA-API. 4:4:4 12-bit stays outside v1. Refactor guardrails enforce FFmpeg/libav direct dependencies only inside `video-ffmpeg`, no `video-vulkan` workspace return, no separate public `ffmpeg_sw` config/UI option, and no public `video.preferred_backend = "vulkan"` except rejection diagnostics/tests.
- Public `video.preferred_backend` config/settings value supports only `auto`, `hardware`, and `software`; current config schema version is 7. Legacy schema v2/v3/v4 loads into the current schema: v2 `vaapi` still loads as `hardware`, and the removed duplicate `video.hardware_decode_only` key is stripped from legacy TOML before strict serde so it is no longer exposed, written, or shown in Settings UI. The old `vulkan` video decode backend preference remains removed, rejected during config deserialization with a suggested `auto`/`hardware` fix, and must not be reintroduced as runtime fallback. `hardware` means native hardware decode path (VA-API today, not VA-API forever). `software` means FFmpeg software decode only; it selects the `FfmpegHostUploadWgpu` app composition plan when capability scanning found a playable `ffmpeg-sw` host-upload output, otherwise it returns a typed unavailable/unsupported selection error without starting VA-API. `render.profile = "vulkan"` and `[render.vulkan]` remain render/surface settings only.
- Capability selection must intersect stream requirement, provider-declared `SupportedVideoOutput` (`backend` + codec-level decode format + `VideoFrameContract`), renderer `VideoFrameContract` support (pixel layout + transfer path + hardware handle layout as one contract, no Cartesian product), DMA-BUF image layout (`DmaBufImageLayout`), and strict color/HDR policy before opening/using video. `BackendCapabilities.raw_supported_outputs` preserves raw provider diagnostics; `SystemCapabilities.playable_video_outputs` stores the renderer-intersected outputs used for stream selection. Capability schema version is 6 after replacing backend format/export side channels with raw/playable supported outputs.
- `app-egui` owns window/UI/lifecycle/backend composition. `main.rs` is a thin process entrypoint; `crates/app-egui/src/app_shell/mod.rs` holds the winit `ApplicationHandler`; `crates/app-egui/src/redraw_pacing.rs` owns redraw pacing plus shell background poll scheduling decisions (`BackgroundPollScheduler` returns `RedrawControlAction`; `AppShell` only computes flags and applies `ControlFlow`/`request_redraw`; continuous playback uses `ControlFlow::Wait` + `request_redraw` (NOT `ControlFlow::Poll`): on Wayland redraw гейтится frame callback-ом и present не блокирует, поэтому Poll крутил event loop вхолостую между 60Hz кадрами и жёг целое ядро на главном потоке (~94% -> ~24%, render остаётся 60fps). Wait + request_redraw даёт тот же 60fps, но loop спит между кадрами); `crates/app-egui/src/render_settings.rs` maps validated `AppConfig` to `render-core` settings; `crates/app-egui/src/system_capabilities.rs` runs the shell-level scan policy (VA-API provider + renderer capabilities); `crates/app-egui/src/video_pipeline_selector.rs` owns pure app-level concrete pipeline selection from committed video config, the last `SystemCapabilities` snapshot, and decoder-thread config intent; `AppState` stores that read-only capability snapshot, sends a clone to `PlayerWorker`, and never runs capability scan during pipeline rebuild; `crates/app-egui/src/startup_media.rs` owns CLI initial media orchestration via `StartupMediaController` (`InitialMedia`, yt-dlp/direct-media startup jobs, startup error). yt-dlp startup is capability-aware: the background job uses the single app-owned S19 -> S21C -> S22 path, selects against probed video and audio capabilities, opens the exact candidate through registered transport/demux providers, and propagates shutdown cancellation into extractor, transport and progressive demux. Direct media startup is service-neutral: CLI routing sends non-yt-dlp `http(s)` media URLs with explicit supported extension to `service-direct-media`, then adapts the neutral result to `PreparedMedia`. `AppState` keeps player snapshot refresh (`refresh_player_snapshot`) separate from explicit desktop integration publication (`publish_desktop_snapshot`); frame/runtime/hotkey call sites publish only when they intentionally preserve desktop side effects. It still must not own playback queues, scheduling, demux state, audio/video decoder state, renderer/GPU internals inside startup media, or `PlayerSession`.
- UI local-file opening is asynchronous inside `app-egui`: `rfd::AsyncFileDialog` and `local_media::prepare_local_file` run through a shell job, then successful caller-prepared media enters the shared `state::strong_media_open` adapter. The adapter waits Ready -> explicit authorization -> Enqueued barrier -> exact Installed before publishing source state; cancel/prepare/pre-barrier errors keep old playback.
- CLI/local restore `load_file(&Path)` may stay synchronous unless the UI file-dialog path is involved.
- `player-core` owns playback state and consumes already-opened `PreparedMedia`; it must not reintroduce direct deps on `symphonia-demux`/`webm-demux` for opening or on `video-vaapi`/other concrete backend crates for backend internals.
- Backend startup/resource-provider boundary lives in `video-backend-api`: `player-core -> video-backend-api`, `video-vaapi -> video-backend-api`, and concrete video backend crates must not depend on `player-core` for adapter contracts.
- Renderer receives playback frames through `video_present_core::VideoFrameLease` returned by `PlayerWorker::try_acquire_present_frame`; scrub visual override frames use the separate S16 `PlayerWorker::try_acquire_scrub_visual_override_frame` handoff. `player-core` does not keep old `PresentFrameLease`/`PlayerPresentFrame` public aliases or re-exports. UI/actions communicate through `PlayerCommand`, `PlayerSnapshot`, and worker events.
- `video-present-core::VideoPresentFrameIdentity` is the stable present-frame identity vocabulary (`render_generation`, `resource_handle`, `decoded_generation`, `pts`) used by app playback cache and scrub override commit/match clearing; do not collapse it to `MediaTime` or resource handle alone.
- Config is user TOML only; cookies, history, bookmarks, durable cache metadata, and service sessions are not config responsibilities. Network prefetch user knobs live in TOML as `network.prefetch_initial_chunk_kb` (default 64 KiB), `network.prefetch_chunk_mb` (default 8 MiB), and `network.read_ahead_mb` (default 256 MiB), but the `media-prefetch` crate itself remains config-agnostic.
- Frame-server guardrail policy is enforced by `scripts/check-refactor-guardrails.py`: `frame-server-core` stays a neutral contract crate with normal deps only on `media-core`/`video-present-core`; `player-core -> frame-server-core` is intentional, reverse deps from `frame-server-core` to player/app/render/concrete backend/service remain forbidden. Since 2026-07-03, hover preview, hover predecode, hover budget diagnostics, timeline hover prepare working set, and renderer hover overlay paths are removed. Frame server remains for SeekLanding/live scrub now and future playback-rate frame serving; do not reintroduce hover-specific lanes, settings, diagnostics, working sets, backend reservations, or app executors. Config schema v7 removes old hover keys; legacy v4 loading may strip them before strict parse only to keep existing user TOML bootable, then migrates to the current schema. Current v7 configs must reject removed hover keys, and cleaned configs must not expose or write them.\n- Project knowledge is maintained in Serena memories; AGENTS.md rules are in Russian and are project-local operating instructions.
- `user/` is the user's personal workspace for prompts, plans, benchmarks, and exploratory notes. Do not treat it as canonical architecture documentation or required reading unless the user points to a specific file or the task explicitly concerns those notes.

- Session 08C controlled live renderer recreation, WGPU queue rebind/device-lost exactly-once releases, typed restore failures and shell serialization are documented in `mem:render-video/controlled-renderer-recreation-s08c`.
- Session 15 DMA-BUF layout capability/descriptor rejection and exactly-once `auto` runtime fallback are documented in `mem:render-video/dma-buf-layout-fallback-s15`; capability schema is version 7 (v7 adds the distinct serialized H.264 `baseline` profile) and `DmaBufImageLayout::ComposedMultiObject` is an explicitly unsupported renderer contract.
- Session 08D completed settings live apply end-to-end: `settings-core` orders validate/preflight/runtime/persist/final commit; `rustiplayer-settings` builds deterministic owner routes and reverse compensation; `app-egui::settings_runtime::transaction` owns orchestration and UI progress/retry state. Runtime config snapshot is synchronized only after successful atomic TOML persistence. Generic deferred/unsupported/requires-rebuild outcomes and persisted-runtime divergence state are removed. Details: `mem:settings-ui/application-contract-s08`.
- Session 27D decomposed settings infrastructure without behavior changes: `settings-derive` now separates parsing/validation/codegen, and `rustiplayer-settings` separates AppConfig transaction binding from project-specific typed routing behind stable facades. Owner map and verification: `mem:settings-ui/infrastructure-decomposition-s27d`.

- Session 16 yt-dlp HDR selection policy, schema-v6 setting ids, typed UnknownDynamicRange rejection and service-owned selection boundary are documented in `mem:media-services/ytdlp-hdr-selection-s16`.

- Session 28 readiness audit (2026-07-12) verdict is `NOT READY` for an unrestricted feature roadmap: correctness/domain boundaries largely pass, but foundation remains blocked by quick-xml RUSTSEC-2026-0194/0195 and the coverage ratchet/test-relocation classification problem. Full reproducible evidence and allowed work directions are in `docs/history/readiness_report_2026-07-12.md` (historical snapshot, not current readiness); details live in `mem:ci/github-actions` and `mem:testing/coverage`. All historical hover memories are already under `archive/...`; hover lanes remain removed.

- Custom egui artwork boundary: see `mem:app-egui/artwork-boundary`; `ui-artwork-egui` owns Painter primitives and shared visual geometry, while `app-egui` owns interaction/accessibility/actions.

- Session 10A typed winit wake bridge, race-safe owner mailboxes и process-lifetime `PlaylistRuntime` lifecycle/shutdown shell документированы в `mem:app-egui/wake-runtime-s10a`; defensive polling больше не является условием доставки startup/local/settings completions.
- Session 15 target-first local open и process-lifetime sibling discovery orchestration документированы в `mem:app-egui/playlist-discovery-s15`; `playlist-core` атомарно выделяет stable IDs только при accepted batch commit, а `playlist-discovery` остаётся policy/queue-neutral.
- Session 15A completed 2026-07-16: app-owned marker router correlates D74 admission/readiness revisions with the active discovery scope; controller-owned D41/D50 waits revalidate exact non-shuffle targets or re-query domain-owned shuffle upcoming; D58 cancels only navigation interest while bulk discovery continues. Marker events never mutate queue/current/history/dirty state, and all released targets retain the D08/D39 Ready→reservation→authorization→Installed commit path. Details: `mem:app-egui/playlist-discovery-s15` and `mem:app-egui/playlist-controller-s12`.
- Session 16 completed 2026-07-16: the single playlist discovery executor now also serves app-owned Manual Add and demand-driven visible metadata jobs with bounded read models; D66 queue generation, D67 capped atomic prefix, duplicate locator IDs, pure URL append, unified D15+D79 process-lifetime confirmation, fingerprint-aware metadata patching, production exact-Installed local/URL cache update, and D70 retained unavailable rows are active. Sensitive-persistence classification stays inside `service-direct-media`; visible refresh performs a no-demux fingerprint check first and revalidates structural revision before patching. D64 target Play remains one actual prepared demux open with no discovery probe pair. Details: `mem:app-egui/playlist-discovery-s15`, `mem:app-egui/queue-replacement-confirmation-s14a`, `mem:playlist/discovery`, and `mem:playlist/core`. Metadata Sort/UI/startup restore remain out of scope; next allowed playlist session is 16A.


- Sessions 10C/10D completed 2026-07-14: process-lifetime `PlaylistRuntime` owns policy-neutral bounded `app-egui::media_open`; D64/D75 local preparation remains single-handle/single-demux, and startup/local/settings production callsites now use one strong completion adapter. Ready never auto-authorizes, Enqueued is only a barrier, exact Installed is route success. Player-selected video candidate mapping, exact-instance position/track restore, D52 confirmation, lossless pre-barrier rejection cleanup and settings correlated reinstall compensation are active. One compatibility facade remains only in focused player tests with TODO removal. Details: `mem:app-egui/media-open-coordinator-s10c`, `mem:settings-ui/application-contract-s08`, `mem:player-core/core`.


- Session 11A completed 2026-07-14: process-lifetime `PlaylistRuntime` now owns modular `PlaylistController`, canonical queue identities, D08/D39 three-phase reservation guard, bounded runtime row errors, dirty signals, stop-after-current storage shell, and Arc-backed revision-stable view snapshots. Renderer-bound `AppState` receives only a validated exact binding plus immutable snapshot attachment. Navigation/Ended/tombstone/discovery/persistence/UI remain out of scope; next allowed work is Session 11B. Details: `mem:app-egui/playlist-controller-s11a`.

- Session 11B completed 2026-07-14: `PlaylistController` owns manual Play/Next/Previous, stable intent/origin, one D50 wait, D17 restart-first, D58-D60 coalescing and exact guard transport drain. `player-core` exposes exact-instance restart/neutral Stop receipts; details: `mem:app-egui/playlist-controller-s11b`.
- Session 11C completed 2026-07-14: one-slot D53-D57 cursor preserves the domain-owned preview/token across fast supersede and typed dispatch winners; details: `mem:app-egui/playlist-controller-s11c`.
- Session 12 completed 2026-07-15: `PlaylistController` now owns exact edge-triggered Ended/Failed, D42/D50/D53-D58 holds, D26 deferred latch shell, D03/D03a Stop/Skip and D49/D59/D70 integration. `playlist-core::queue::automatic` owns opaque fixed committed snapshot plans/tokens and atomic shuffle RepeatQueue commit; player-core semantics/API are unchanged. Discovery/store/config/UI and tombstone/removal remain out of scope; next allowed work is Session 12A. Details: `mem:app-egui/playlist-controller-s12`.


- Session 12A completed 2026-07-15: destructive Remove/Clear/RemoveOthers now detach active media into a runtime tombstone while persisted current becomes None; `PlaylistRuntime` owns one 8-second shared-snapshot removal Undo slot with exact lineage/deadline rules. Domain snapshot/current contracts are in `mem:playlist/core`; controller/runtime lifecycle is in `mem:app-egui/playlist-controller-s12a`. UI/store/discovery and real D72 checkpoint wiring remain later sessions; next playlist session is 13.

- Session 14 completed 2026-07-15: typed process bootstrap/lease, closed startup allocator gate, process-lifetime playlist-state persistence and one-deadline terminal shutdown are documented in `mem:app-egui/playlist-persistence-s14`. Renderer suspend preserves playlist/store/lease; restored-current open remains Session 17.


- Session 14A completed 2026-07-15: process-lifetime `PlaylistRuntime` owns D79 destructive replacement confirmation; local picker is separated from media preparation, in-app/trusted startup origins are distinct types, and the safe entity reuses the existing center overlay. No in-app local/URL media or discovery I/O reaches lower layers before matching Confirm on a nonempty committed queue. Details: `mem:app-egui/queue-replacement-confirmation-s14a`.

- Session 14B completed 2026-07-15: process-lifetime `PlaylistRuntime` owns the non-persistent active-media checkpoint; resume is strictly StartPaused -> exact seek/typed non-seekable -> stable intent -> same-lineage rebind. Exact player release and external strong-install lineage registration prevent stale cleanup/identity commits. Details: `mem:app-egui/suspend-resume-checkpoint-s14b`.

- Session 16A completed 2026-07-16: `bounded-work-executor` is a new neutral workspace asset for fixed workers, bounded non-blocking admission, typed results/backpressure/panic outcomes, cooperative cancellation, and shutdown/join. Accepted tasks may install an exactly-once terminal notifier that runs after result publication for success/panic/cancel-before-start; notifier panic is isolated. Self-drop on a worker detaches only its own unjoinable handle and still joins all other workers. It has no playlist/UI/filesystem dependencies. `app-egui` uses it for background canonical Sort key preparation and event-driven terminal wake; transactional Sort and preflighted D44 salvage details are in `mem:playlist/core` and `mem:app-egui/playlist-discovery-s15`. Next allowed playlist session is 17; UI wiring was not started.

- Session 17 completed 2026-07-16: process-lifetime restore/CLI startup winner, protected allocator generations, paused restored fallback, bounded D65 retained actions и nonblocking stepwise strong install документированы в `mem:app-egui/startup-orchestration-s17`. CLI local sibling discovery стартует только после exact Installed; restore siblings не сканируются. Sessions 18, 18A, 18B и 19 завершены: read-only virtualized Playlist UI, main transport/hotkeys/global wait/Undo, process-lifetime desktop/MPRIS и typed toolbar/forms/progress adapters подключены без переноса traversal/policy из controller. Session 19 boundaries документированы в `mem:app-egui/playlist-ui-s19`; следующий playlist scope — Session 20 row interactions.


- Session 18B completed 2026-07-16: desktop/MPRIS backend and revisioned transport owner moved from renderer-bound `AppState` into process-lifetime `PlaylistRuntime/AppShell` after D10e lease. Desktop commands are neutral/bounded/wake-driven; controller owns navigation/modes/Stopped, app owns effective volume and track lineage, and player-core owns exact correlated timeline seek. `desktop-integration` no longer depends on player-core and alone owns zbus/object paths. Full contract: `mem:app-egui/playlist-desktop-transport-s18b`.
- Playlist Session 21 completed 2026-07-16 with a feature-scope PASS. D01–D81 and all 39 prerequisite sessions are traced in `user/playlist_queue_session_21_traceability.md`; Session 20 row interactions remain documented in `mem:app-egui/playlist-controller-s20` and `mem:app-egui/playlist-ui-s20`. No production Rust/API/boundary change was needed during final hardening; `player-core` remains queue-free. Repository foundation is still NOT READY for the two known D28 classes (quick-xml advisories and coverage relocation/ratchet), and manual release smoke is NOT RUN until explicit user fixtures are supplied. Final details: `mem:app-egui/playlist-final-s21`.


## Playlist main Open UX correction (2026-07-17)
- Main in-app single-file Open is directory-aware: an exact committed item in the current lexical parent directory reuses its stable Item ID/Row Play; a different directory or missing exact/current local identity follows typed atomic queue replacement. Playlist `Add Files` remains append-only.
- In-app directory replacement installs the chosen target paused as the D08 anchor, waits only until the natural queue beginning is proven, then restarts the target if it is first or strong-installs the first committed row. CLI target startup remains immediate. Exact identity/revision guards make later user transport/structural/lifecycle actions win over deferred autoplay. Details: `mem:app-egui/playlist-discovery-s15` and `mem:app-egui/queue-replacement-confirmation-s14a`.


- yt-dlp playlist title enrichment completed 2026-07-17: service-owned cancellable `yt-dlp` metadata summary, app-owned bounded process-lifetime jobs, immediate post-append plus visible/restore enrichment, exact Item ID+locator patching, and post-Installed title reuse are documented in `mem:app-egui/ytdlp-playlist-metadata-2026-07-17`.


## Stop-after-current removed (2026-07-18)
- Пользовательская one-shot фича «После текущего» удалена целиком, а не только скрыта из UI. `app-egui` больше не хранит `StopAfterCurrentLatch`, не публикует `PlaylistAction::SetStopAfterCurrent`, не имеет D58 deferred transport/outcome/EOF policy и не отменяет navigation/install ради этой команды.
- Обычные queue modes не изменены: `RepeatMode::{StopAtEnd, RepeatQueue, RepeatOne}`, автоматический переход, manual Next/Previous, explicit Stop, D50/D56/D57 и D26 deferred cancellation продолжают работать через существующие owner boundaries.
- Публичные enum-ы `player_core::MediaInstallCancellationCause` и `playlist_discovery::DiscoveryCancellationCause` больше не содержат `StopAfterCurrent`; остальные typed cancellation distinctions сохранены. Focused и full all-features workspace suites, strict Clippy, fmt, locked Rust 1.96 check и refactor guardrails прошли. Детали: `mem:app-egui/stop-after-current-removed-2026-07-18`.


## Web media roadmap S01P/S01Q queue read boundary (2026-07-20)
- `playlist-core` получил future-proof read boundary без обещания contiguous queue storage: `iter_playable_items()`, `iter_playable_ids()`, stable-ID `item()`, intent counts `top_level_entry_count()`/`retained_item_count()` и immutable `OwnedPlayableItemsSnapshot` для async/persistence ownership handoff.
- `playlist-core` internal algorithms/tests и `playlist-state` DTO/snapshot migration завершены без изменения canonical order, Item IDs, revisions или playlist-state schema v1.
- S01Q завершил workspace migration: app view/selection/discovery/startup/desktop-MPRIS и diagnostics/tests используют intent-based iteration, stable-ID lookup и named counts; `PlaylistQueue::items()` и ambiguous production `len()` удалены. Selection ranges и removal fallback сохраняют stable-ID/revision authority, не используют queue slice для structural mutation; новых cached/parallel Vec или app-owned queue snapshots нет.
- S01Q verification: `playlist-core` 82, `playlist-state` 40 и полный `app-egui` 719 tests PASS на Rust 1.96; strict Clippy, workspace all-features check, MSRV 1.92, rustfmt, Serena diagnostics и refactor guardrails PASS. Cargo-deny по-прежнему падает только на известные quick-xml RUSTSEC-2026-0194/0195. Details: `mem:playlist/core`, `mem:playlist/state`.

## Web media roadmap S01A compound storage (2026-07-20)
- `playlist-core` canonical storage теперь first-class `Single | Compound`: отдельный monotonic/no-burn Group ID allocator, structural `PlaylistEntryId`, ordered stable-ID parts с immutable ordinal, root locator provenance и cached group summary.
- Новый atomic entry append/replace preflight публикует Item/Group watermarks только вместе; retained capacity считает parts, group-safe capped prefix никогда не режет compound, empty draft typed rejected, one-part group остаётся compound.
- S01P reads работают поверх nested storage без queue cache: top-level и derived playable iteration — разные named views; owned flat snapshot создаётся только для explicit handoff. Metadata-only patches применяются к nested parts без structural/traversal revision.
- S01B/S01C group-safe structural/shuffle/navigation scope не заявлен готовым; player identity/UI collapse state не переносились в core. Full contract и verification: `mem:playlist/core`.


## Web media roadmap S01B group-safe structural mutations (2026-07-20)
- Все current structural mutation boundaries `playlist-core` теперь адресуют top-level `PlaylistEntryId`: single/bulk remove, remove-others, single/multi move, relative anchors, discovery insertion anchors и direct/prepared canonical sort. `PlaylistEntryId::Single(part_id)` получает typed compound-part rejection и никогда не мутирует subordinate part отдельно.
- Sort готовит один key на top-level entry: Single использует item metadata/locator, Compound — cached group summary/root provenance; permutation переставляет entries и сохраняет exact part order/current Item ID. Prepared sort хранит expected/sorted Entry IDs, а metadata patches остаются Item-ID адресованными.
- Removal Undo принимает только exact order-preserving deletion result, восстанавливает exact Item/Group IDs и отвергает unrelated reorder с той же revision delta. App structural selection preflight требует полного покрытия compound и линейно (`O(N+K)`) переводит playable selection в explicit Entry IDs.
- Discovery anchor/read hint vocabulary использует `PlaylistEntryId`; текущие local-discovery commits остаются explicit Singles. Navigation, reservation, group-block shuffle traversal, UI compound presentation и persistence v2 остаются S01C/следующими scope. Details: `mem:playlist/core`, `mem:playlist/discovery`.


## Web media roadmap S01C part traversal/group-block shuffle (2026-07-20)
- `PlaylistQueue` current остаётся exact playable `PlaylistItemId`; canonical manual/automatic traversal проходит ordered compound parts, затем следующий top-level entry. `RepeatOne` повторяет exact part, `RepeatQueue` оборачивает derived traversal.
- Shuffle теперь разделяет identity по смыслу: factual history хранит только реально Installed part Item IDs, upcoming хранит duplicate-free top-level `PlaylistEntryId`. Current compound entry исключается как block; из middle part Next сначала проходит remaining source-order parts, Previous использует только factual history, новый cycle входит в compound с первой part.
- Append/discovery merge, remove/current detach, sort и move работают с group-block upcoming; fixed automatic failure chain пытается remaining parts по порядку без fake visits, late-entry admission или hidden group split. Ready/D08 reservation не меняет current/history; abort/stale/cancel сохраняют base, exact Installed коммитит current и opaque shuffle delta вместе. App controller focused test подтверждает exact part publication после matching Installed.
- Public `ShuffleTraversalSnapshot::upcoming()` и restore errors теперь используют Entry IDs. Schema v1 `playlist-state` мапит legacy single upcoming в `PlaylistEntryId::Single` и typed-ошибкой `CompoundQueueRequiresSchemaV2` запрещает lossful compound flatten до запланированного S02.
- Verification: 106 `playlist-core`, 41 `playlist-state`, 722 `app-egui` tests PASS; strict touched all-targets/all-features Clippy, Rust 1.96 workspace check, focused MSRV 1.92, rustfmt, refactor guardrails, diff check и Serena diagnostics PASS. Details: `mem:playlist/core`, `mem:playlist/state`.


## Web media roadmap S01D neutral playlist payloads (2026-07-20)
- `playlist-core` добавил neutral checked playback spans, bounded ancillary/import provenance, versioned/redacted `DurableReopenLocator` и ID-less Single/Compound import drafts без queue algorithm/persistence integration. Stable service child identity допускается только как bounded v1 webpage/original/extractor payload; format/manifest/fragment/key/signed endpoint/headers/cookies/auth/session material typed rejected. Full contract: `mem:playlist/core`.


## Web media roadmap S01G compound-core hardening gate (2026-07-20)
- S01G PASS без production/API/dependency changes: полный Serena audit подтвердил, что все `PlaylistQueue` mutators остаются у domain owner-а, app вызывает их только из serialized controller/runtime owner turns, а `playlist-discovery` не получил queue authority.
- Добавлены focused proofs совместного Item/Group allocator high-watermark через Clear/compound replace/reserved single replacement, точного mixed-end `ExactSizeIterator` поверх compound storage и автоматического storage/module-size guardrail. Legacy slice/ambiguous queue len API и cached flat queue по-прежнему отсутствуют.
- Verification: 122 playlist-core, 41 playlist-state, 722 app-egui tests PASS; strict Clippy, workspace check, MSRV 1.92, fmt, refactor guardrails, diff check и Serena diagnostics PASS. Cargo-deny падает только на прежние quick-xml RUSTSEC-2026-0194/0195. Full handoff: `mem:playlist/compound-hardening-s01g-2026-07-20`.

## Web media roadmap S03 neutral values (2026-07-20)
- Добавлен dependency-free `web-media-core`: service/runtime-neutral source/candidate/semantic identities, bounded redacted raw+parsed transport/container/codec values, typed muxed/separate/video-only/audio-only layouts, video/audio/subtitle descriptors, `BestPlayable`/`Exact`, deterministic preferred-height rank и static compatibility rejection vocabulary.
- Semantic refresh identity source-scoped; candidate boundary отвергает cross-source semantic/subtitle identities. Unknown S00 identities сохраняются exact до 256 UTF-8 bytes без diagnostics leakage; provider/process/HTTP runtime/config/UI types отсутствуют.
- Public contract, bounds, tests и verification подробно записаны в `mem:media-services/core`. Новый crate пока не подключён к `service-ytdlp`; mapping принадлежит будущей S19.

## Web media roadmap S04 neutral atomic file durability (2026-07-20)
- Добавлен std-only `atomic-file-store`: единый neutral owner create-new same-directory temp/Unix 0600/write/flush/file-sync/rename/directory-sync/exact-path RAII cleanup без playlist/app/config knowledge и без wildcard policy.
- `playlist-state -> atomic-file-store`; `playlist-state` сохраняет JSON serialization, общий operation mutex с inspection/quarantine, worker retry policy и прежний public outcome API. Queue-state и resume sidecar мигрированы behavior-neutrally. Полный контракт и verification: `mem:playlist/state`, app ownership: `mem:app-egui/playlist-persistence-s14`.


## Web media roadmap S04X hardened XML boundary (2026-07-20)
- `bounded-xml-reader` теперь владеет единым byte-slice-only untrusted XML boundary с обязательными caller-defined byte/depth/token/attribute/text/namespace budgets, project-owned events и отсутствием hidden I/O. DTD/DOCTYPE, external/custom entities, undeclared prefixes и XML 1.1 rejected; predefined/numeric XML 1.0 entities legal.
- Transitively vulnerable `quick-xml 0.39.3` заменён на `0.41.0`; exact published `wayland-scanner 0.31.10` временно закрыт пятым локальным `[replace]` patch без window/UI stack migration. Cargo-deny advisory graph clean. Full contract: `mem:xml/core`; patch ownership/removal gate: `mem:dependency-patches/core`.


## Web media roadmap S05 M3U/M3U8 + HLS distinction (2026-07-20)
- Добавлен neutral `playlist-io`: byte-slice-only `M3uParseRequest` с explicit M3U/M3U8 intent, document source и budgets; generic preview возвращает ID-less `playlist-core` drafts, bounded issues и exact EXTINF hints без queue/I/O/service authority.
- Content-first strict HLS pass выполняется до generic EXTINF interpretation: network HLS возвращает только `AdaptiveManifestReference`, local HLS — `LocalHlsManifestUnsupported`; segment URI никогда не становятся queue rows. RFC UTF-8/BOM/NFC/control/case/whitespace/attribute/topology invariants и secret-safe exact URL/base/file resolution покрыты focused tests.
- Полный contract/verification/known scope: `mem:playlist/io-s05-m3u-hls-2026-07-20`.

## Web media roadmap S06 secure XSPF v1 (2026-07-20)
- `playlist-io` теперь владеет streaming namespace-aware XSPF v1 schema/model поверх единственного hardened `bounded-xml-reader` boundary: exact namespace/version, ordered/cardinality-checked trackList/tracks, inherited document/local `xml:base`, ordered 0..N location candidates без service admission и metadata duration-only hint.
- Versioned Rustiplayer extension `urn:rustiplayer:xspf:playlist-extension:1` использует одну playlist-level minimal group запись (`firstTrack`, `trackCount`, root location) без per-track duplication; ranges валидируются линейно после flattened trackList.
- Добавлен typed export URI eligibility без queue serializer: reversible percent-encoded native/file URL либо secret-safe rejection foreign/unrepresentable/service-owned identity. Full contract и verification: `mem:playlist/io-s06-xspf-2026-07-20`.

## Web media roadmap S07 bounded nested local expansion (2026-07-20)
- `playlist-io` стал общим local-only recursive owner для M3U/M3U8/XSPF includes: deterministic DFS tree, active-stack-only canonical cycle identity, reversible stored native/non-UTF paths, aggregate depth/document/byte/item/diagnostic budgets и per-request cancellation between documents.
- Network playlist-looking URL остаётся leaf без fetch; local HLS typed unsupported. XSPF multi-location admission остаётся будущей S08 policy; recurse выполняется только для одного unambiguous local file candidate. Full contract: `mem:playlist/io-s07-nested-local-expansion-2026-07-20`.

## Web media roadmap S08 source-neutral import transaction (2026-07-20)
- `PlaylistRuntime` владеет одним latest-only source-neutral import preview/staged transaction с generation + structural-revision revalidation; typed intents — AppendToQueue, interactive ReplaceQueue и trusted StartupReplace. Partial/truncation принимаются explicit Continue, затем aggregated sensitive durable-locator + replacement reasons занимают прежний единый `PendingPlaylistConfirmation` slot в deterministic order.
- Whole-entry capped prefix не режет compound; import materialization остаётся ID-less, а Item/Group IDs публикуются только domain append/replace commit-ом. App XSPF registry выбирает первый admissible ordered location без open/probe.
- Interactive Replace не использует Clear/removal continuation: old active media detach-ится, clean Ended даёт exactly-once Stop, Next/Previous выбирают first/last source-order target с compound/shuffle accounting и без hidden failure scan. Full contract: `mem:app-egui/playlist-import-s08-2026-07-20`.

## Web media roadmap S09 Import toolbar и preview UI (2026-07-20)
- Icon-only toolbar получил Import пятым left slot после неизменных Add Files/Add URL/Sort/Current Item axes; 32-point row и независимый Clear anchor сохранены. Menu публикует typed append/replace intent после render; новый neutral Import glyph принадлежит `ui-artwork-egui` и визуально отличается от Add Files.
- Process-lifetime `PlaylistImportIoOwner` владеет одним async native single-root picker и bounded worker: filters только M3U/M3U8/XSPF, CUE отсутствует, а authoritative validation/expansion остаётся в `playlist-io`. Pure materializer строит S08 ID-less draft; UI не выполняет I/O/parser/queue mutation.
- Preview показывает clean/partial/issues/truncation/sensitive/replace состояния и возвращает typed Continue/Cancel; existing composed confirmation остаётся единственным authoritative confirmation slot. Supersede подавляет также completion, уже опубликованный worker-ом, но ещё не применённый owner-ом. Full contract: `mem:app-egui/playlist-import-s09-2026-07-20`.


## Web media roadmap S10 pure M3U8/XSPF export core (2026-07-20)
- `playlist-io` теперь владеет immutable canonical top-level export snapshot, service/app-owned portable locator preflight, aggregated secret classification и pure UTF-8 M3U8/XSPF v1 serializers без filesystem/UI/runtime authority.
- Selected compound экспортирует все source-order parts; M3U8 публикует typed flatten warning, XSPF сохраняет group range/root через versioned playlist extension. Local relative representation используется только при proven roundtrip; non-UTF/foreign/nonportable service identity typed rejected; transient transport material отсутствует в API.
- Verification и полный contract: `mem:playlist/io-s10-export-2026-07-20`. `playlist-state` и queue mutation/public storage semantics не изменены; следующий scope — S11 writer/runtime/UI.


## Web media roadmap S11 export runtime/writer/toolbar (2026-07-20)
- Process-lifetime `PlaylistRuntime` получил отдельного `PlaylistExportIoOwner`: maximum-one save-dialog/preflight/write job, monotonic generation, cooperative cancel, stale suppression, panic correlation и bounded shutdown/join. Immutable S10 snapshot снимается до background work; queue/current/selection/dirty state не мутируются.
- Toolbar Export идёт шестым left slot после Import и требует typed scope + M3U8/XSPF до save dialog. Empty queue отключает control, empty selection — только selected branch. Sensitive locator preflight создаёт redacted continuation в generalized confirmation slot; late completion не вытесняет более новый prompt.
- Готовые bytes записываются единственной app boundary-функцией через `atomic-file-store`, с typed overwrite intent от native save dialog, Unix 0600 и сохранением `NotReplaced`/`ReplacedDurabilityUnconfirmed`/`Durable`. URL sensitivity принадлежит service-owned policy; stable service payload export остаётся fail-closed до S19 owner mapping.
- Neutral Export glyph принадлежит `ui-artwork-egui` и образует characterized tray-пару с Import. Полный contract/verification: `mem:app-egui/playlist-export-s11-2026-07-20`.


## CUE S14 boundary (2026-07-20)
Exact CUE export identity теперь durable в `playlist-core`, additive persisted в playlist-state v2 и проецируется app-egui в neutral playback window; player-core остаётся CUE-free. Pure eligibility/serializer живут в playlist-io, UI кеширует full/selected availability по view revision. Детали и тесты: `mem:app-egui/cue-integration-s14-2026-07-20`.


## Web media roadmap S15 bounded yt-dlp topology (2026-07-20)
- `service-ytdlp` получил public owned `Video | Playlist | MultiVideo | Delegation` topology boundary с unavailable rows, explicit `url`/`url_transparent` merge policy и без nonserializable runtime state.
- Official CLI profile использует lazy line-delimited `--dump-json` + authoritative final `--dump-single-json`, `--flat-playlist --lazy-playlist`; `n_entries` не authoritative. Production system config/cookies/plugins остаются trusted external code, hermetic fixtures изолированы.
- Process owner enforce-ит stdout/stderr/JSON-line/entry/JSON-depth/topology-depth/field budgets и kill+wait на cancellation/timeout/overflow; raw locator/direct endpoints/stderr redacted. Full contract: `mem:media-services/ytdlp-topology-s15-2026-07-20`.

## Web media roadmap S16 app topology draft mapping (2026-07-20)
- `app-egui::url_topology_drafts` чисто маппит уже extracted `YtDlpTopology` в ordered ID-less `PlaylistImportEntryDraft` + bounded safe issues без queue/allocator/commit/I/O authority и без второго URL parser-а.
- Video -> Single, collections nested-flatten, MultiVideo -> first-class Compound; one part остаётся compound, zero retained parts даёт issue/no draft, stable unavailable сохраняется, missing identity становится issue, duplicates/order сохраняются.
- Exact root остаётся exact durable URL; extracted child/delegation identity сохраняет service-ytdlp ownership через versioned stable service payload, поэтому direct-media-first не перехватывает reopen. Ephemeral transport material отсутствует/fail-closed. Full contract: `mem:app-egui/ytdlp-topology-drafts-s16-2026-07-20`.


## S17M compound MPRIS/external projection (2026-07-21)
- Process-lifetime desktop owner теперь публикует exact compound part projection и fence-ит player-dependent external commands стабильной `DesktopControlRevision`, повторно проверяя queue revision/current, active part, media instance и player binding generation. Group header не является MPRIS track; metadata использует part title и bounded redacted group context. Полный контракт: `mem:app-egui/playlist-desktop-transport-s18b`.


## S18 playlist/topology hardening gate (2026-07-21)
- Milestone playlist/topology доказан без новой feature logic: canonical top-level consumers, отсутствие legacy queue slices/parallel canonical Vec, secret-safe presentation, full format/topology/runtime/persistence matrix и verification описаны в `mem:playlist/topology-hardening-s18-2026-07-21`.
- Coverage inventory теперь включает `atomic-file-store`, `bounded-xml-reader`, `playlist-io`, `web-media-core`; `web-media-core` закреплён как std-only neutral contract. Coverage baseline не переписывался из-за отдельного известного relocation blocker-а.


## Workspace boundary: demux-api + event-first read contract (S21/S21R, 2026-07-21)

Workspace содержит neutral `crates/demux-api` между `media-core`/`source-core` contracts и concrete `symphonia-demux`. Он владеет typed demux input/probe/registry и generic composite A/V semantics; подробности см. `mem:demux-api/core`. `media_core::Demuxer` теперь имеет единственный required generic read method `next_event`: generic `next_packet` удалён, finite/legacy `Option<Packet>` mapping централизован в `finite_packet_read_event`. `DemuxReadEvent::TemporarilyUnavailable(DemuxRetryHint)` отделён от packet/EOF/error; hint валидирует earliest retry в safety bounds 1 ms..60 s. Composite держит не больше одного validated pending packet на component, применяет bootstrap packet/byte и timestamp lead caps, выбирает minimum retry required unavailable sides и завершает EOS только после terminal state обеих required components. Player tick в S21R прекращает текущий read pass без EOF/error mutation; exact generation-fenced deadline и resumable staged preflight остаются S21W. Dependency guardrails, cargo-machete inventory и blocking coverage policy включают `demux-api`.

## S21T neutral web transport boundary (2026-07-21)

- Добавлен `web-media-transport-api`: neutral exact provider/component/open/refresh contract, generation fencing, VOD/live + seekable/streaming result shape, validated redirect policy и origin/path/secure-scoped ephemeral secrets. `source-core` теперь владеет checked HTTP target/origin/path/header values и cancellation-aware `StreamingByteSource`; concrete HTTP provider/cache/prefetch/demux/player integration не добавлены. Полный контракт и verification: `mem:media-services/web-transport-s21t-2026-07-21`.


## S22 architecture delta (2026-07-22)

Progressive direct HTTP is now a complete neutral path: `source-core::HttpSourceSession` -> `web-media-http` S21T provider -> `demux-api` registry/progressive worker -> existing player lifecycle. Direct-media classification/privacy stay unchanged; yt-dlp does not own concrete HTTP. See `mem:media-services/progressive-http-s22-2026-07-22`.


## S27 progressive/web hardening gate (2026-07-22)
- Hermetic milestone evidence, architectural guardrails and secret-safe opt-in manual runner are documented in `mem:media-services/progressive-web-hardening-s27-2026-07-22`.
- Production Rust/API/dependencies did not change; actual network/GUI acceptance remains explicit-user-URL-only and `MANUAL REVIEW REQUIRED`.


## YouTube A/V completeness fix (2026-07-26)

- `BestPlayable` ranks complete `Muxed`/`Separate` A/V before single-component candidates, and progressive yt-dlp composite packet retention uses an independent bounded 4 MiB limit rather than the 64 KiB HTTP bootstrap chunk. This fixes silent `VideoOnly` selection and the subsequent real-keyframe composite fatal. Exact selection and single-component-only media remain supported. Full evidence: `mem:media-services/ytdlp-av-completeness-2026-07-26`.


## x36xhzz HLS seek/resume root correction (2026-08-24)

Manifest-owned worker receipt, streaming ingress и resource-bounded MPEG-TS probe устраняют полный body gate и cutoff посреди interleaved AAC PES. HLS-owned `HlsVodSeekLandingPolicy` теперь выбирается до manifest segment selection: default `DecodeFromOrBeforeTarget` сохраняет yt-dlp HLS VOD semantics, `PreferPostTargetRap` явно включает только native finite HLS VOD; live остаётся отдельным path. Player отвечает за actual landing/readiness/presentation/audio, а не за разрешение уже выполненного demux skip. Real release x36xhzz acceptance подтверждает cold resume 355 -> actual 360.033 s, video/audio/progress, warm/restart/supersede/timeline drag и восстановление профиля. Полный handoff: `mem:media-services/hls-vod-manifest-receipted-seek-2026-08-24`, probe details: `mem:media-services/hls-ts-resource-bounded-initial-probe-2026-08-24`.
