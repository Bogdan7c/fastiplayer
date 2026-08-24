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

## Follow-up: прежняя “transport limitation” исправлена (2026-08-24)

- Измеренные до исправления 15–31 s остаются историческим наблюдением, но больше не считаются доказательством transport bottleneck-а. На checkpoint 355.251 s target segment был выбран правильно, а authoritative failure пришёл из MPEG-TS open: bounded 4096-packet initial probe остановился в середине длинного interleaved AAC PES и ложноположительно сообщил malformed/truncated PES.
- HLS app composition теперь связывает initial MPEG-TS probe с уже validated maximum segment resource byte budget через typed owner API. Generic/local MPEG-TS default и strict truncated-resource semantics не изменены.
- Полная буферизация одного target segment остаётся характеристикой текущего HLS resource path, но после parser fix реальный receipt на том же источнике был принят примерно за 0.2 s, landing anchor 350.033 s дошёл до render/play, а холодный resume сохранил `Playing`. Поэтому streaming/range redesign не является prerequisite для исправления этого бага.
- Детали и regression evidence: `mem:media-services/hls-ts-resource-bounded-initial-probe-2026-08-24`.

Related: `mem:core`, `mem:player-core/core`, `mem:demux-api/core`, `mem:media-services/hls-vod-s32c-2026-07-23`, `mem:media-services/hls-vod-seek-index-compaction-aud010-2026-08-23`.