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

## Проверка

- `cargo +1.96.0 test -p media-core -p demux-api -p web-media-hls -p player-core --all-targets --locked`: PASS; media-core 55, demux-api 45, player-core 648, HLS unit 44 и все HLS integration targets.
- Strict affected Clippy `-D warnings`, workspace all-target check, fmt, `git diff --check` и refactor guardrails: PASS.
- Real GUI/MPRIS run на x36xhzz: targets/anchors `279.659 -> 270.033`, `337.698 -> 330.033`, `282.848 -> 280.033`; каждый request прошёл worker-receipted route, приземлился без `DemuxError` и не сканировал от нуля.

## Follow-up: UI scrub release (2026-08-24)

- Реальный timeline drag использует `BeginScrub -> PreviewScrub -> EndScrub`. Даже после исправления one-shot seek `EndScrub` повторно использовал matching live `SeekLanding`, поэтому preview anchor становился final и `active_seek_presents_preroll_progressively()` показывал все кадры от него до цели. Это и было видимым «прокручиванием».
- Инвариант player-core: если `PreparedDemuxSeekRuntime::routes_one_shot_seek_through_worker()` истинно, release scrub-а не имеет права коммитить matching preview route. Он запускает final async seek transaction, которая сначала supersede-ит старый live landing и снимает progressive-presentation policy, затем ждёт authoritative worker receipt. Preview во время удержания мыши для legacy/local media не изменён.
- Functional regression `worker_receipted_scrub_release_suppresses_preroll_until_target_frame_presentation` проходит production command chain, receipt, demux packets, decoder и presentation. Он доказывает ровно один worker request, отсутствие второго sync seek-а и отсутствие любого видимого pre-target frame после release.
- Реальный GUI smoke на приложенном `user/web-media-playlist-acceptance.xspf`: мышью перемещено примерно `00:13 -> 05:17`; UI удержал target, suppressed `Seek discard, expected` вырос до 436, первый target-frame появился примерно через 1 s, затем позиция нормально пошла `05:17 -> 05:19 -> 05:22` без прокрутки от начала.
- После follow-up полный affected test run: media-core 55, demux-api 45, player-core 649, HLS unit 44 и все HLS integration targets; strict Clippy, workspace all-target check, fmt, diff-check, guardrails и Serena diagnostics — PASS.

## Source-scoped landing policy и финальный acceptance (2026-08-27)

### Исправленная граница

- Корневая ошибка подтверждена: post-target выбор выполнялся в HLS manifest/demux до source-specific player wiring и поэтому потенциально менял yt-dlp HLS VOD вместе с native HLS.
- `web-media-hls::HlsVodSeekLandingPolicy` теперь принадлежит HLS open boundary и типизирован: default `DecodeFromOrBeforeTarget`, opt-in `PreferPostTargetRap`.
- Policy входит в `HlsVodOpenPolicy` и передаётся до content probe, initial restore и worker-receipted manifest seek. `initial_open`, `epoch_demux::initial` и `epoch_demux::manifest_seek` читают её до необратимого выбора segment-а.
- Единственный production opt-in находится в `app-egui::web_media_hls_open::prepare_native_hls_vod`. yt-dlp HLS VOD использует общий default request, live HLS остаётся на отдельном live demuxer path; URL heuristics и positional bool отсутствуют.
- HLS отвечает только за manifest segment/RAP и доказанный actual landing. `PreparedDemuxSeekLandingPolicy` player-а остаётся отдельной границей интерпретации receipt, readiness, timeline и presentation; он не может задним числом разрешить demux skip.
- Если post-target RAP отсутствует/не доказывается, opt-in сохраняет decode-forward fallback; default вообще не пробует post-target candidate.

### Regression anchors

- `web-media-hls/tests/receipted_manifest_seek.rs::default_receipted_seek_keeps_containing_segment_decode_forward_semantics` доказывает default containing-segment landing до target; opt-in tests отдельно доказывают post-target RAP, separate A/V и fallback.
- `web-media-hls/tests/initial_restore_runtime.rs` разделяет default containing restore и explicit post-target restore.
- `web-media-hls/tests/runtime.rs::opt_in_vod_worker_seek_uses_post_target_raps_across_discontinuity` закрепляет explicit policy через discontinuity.
- `web-media-hls/tests/transient_manifest_retry.rs::cancellation_of_discontinuous_partial_body_prevents_stale_publication_and_restart` блокирует partial body первой post-discontinuity epoch, supersede-ит её backward seek-ом в предыдущую epoch и доказывает: старый request получает именно `Superseded`, не рестартует HTTP body и не публикует packet; первый committed video packet имеет PTS актуального landing `40 s`.
- `app-egui::web_media_hls_open` tests закрепляют default для shared yt-dlp/live policy и native-only app opt-in.
- `player-core/src/session/tests/post_target_landing.rs` проходит production player command/receipt/decoder/presentation/audio boundaries: receipt сам по себе не завершает seek, frame доходит до presentation, actual landing обновляет position, audio proof обязателен, superseded generation не коммитится.

### Подтверждённый real-GUI x36xhzz acceptance

- Release build прошёл. Все real runs использовали `https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8`, release GUI, MPRIS и production video/audio/render path.
- 3 cold start без checkpoint: process-to-ready `532/370/550 ms`, p50 `532`, p95/max `550`; первый кадр начинался около `33 ms`, начало VOD не перескакивало.
- 3 cold resume с requested `355.000 s`: process-to-ready `473/447/547 ms`, p50 `473`, p95/max `547`. Во всех runs demux actual `360.033 s`, presented frame `360.050 s`, audio proof через `29–30 ms` после landing transaction и последующий position progress.
- 10 отдельных warm seek: ready `34/215/35/247/216/35/35/265/295/35 ms`, p50 `35`, p95/max `295`. Targets `60/180/355/550 s` landed соответственно около `60.033/180.033/360.033/550.033 s`; video presentation, audio proof и progress подтверждены в каждой пробе.
- 3 seek после полного process restart: `60 s -> 352 ms`, `355 s -> 228 ms`, `550 s -> 537 ms`; p50 `352`, p95/max `537`.
- Rapid `550 s -> 60 s`: старый generation получил `SUPERSEDED` без receipt/presentation/commit/progress, новый завершился за `28 ms`.
- Реальный KWin EIS timeline drag через четыре preview-точки: request `355.031 s`, actual `360.033 s`, video/audio/ready `563/565/565 ms`, progress `590 ms`; scrub begin-to-first-preview `0 ms`, begin-to-end `33 ms`.
- `x36xhzz` media playlist не содержит `#EXT-X-DISCONTINUITY`, поэтому real-stream discontinuity не заявляется; он подтверждён synthetic integration test-ом выше.
- Профиль после каждого контролируемого набора и финально восстановлен byte-for-byte к исходным `playlist-state.json`/`playlist-resume.json`; current item 2, checkpoint `41.401578841 s`, fingerprint совпали. Durable memory не хранит временный backup path.

### Verification и известный gate

- PASS: focused/full affected Rust tests, `app-egui --no-default-features`, strict affected all-target Clippy `-D warnings`, workspace all-target check, release build, rustfmt, diff-check, refactor guardrails, Python diagnostics/acceptance tests и strict real-log analyzer.
- Канонический `scripts/pre-pr-checks.sh` запускается, но останавливается на S42 module-size guardrail: десятки уже изменённых ранее файлов большого незакоммиченного worktree превышают/меняют legacy line-count baseline. Не обновлять baseline и не начинать массовую нарезку модулей внутри HLS policy salvage; это отдельный явный архитектурный cleanup/gate.
- Изменения остаются незакоммиченными.
Related: `mem:core`, `mem:player-core/core`, `mem:demux-api/core`, `mem:media-services/hls-vod-s32c-2026-07-23`, `mem:media-services/hls-vod-seek-index-compaction-aud010-2026-08-23`.