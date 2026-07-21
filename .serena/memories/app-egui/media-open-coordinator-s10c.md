# Session 10C: reusable media-open coordinator и single-open envelope (2026-07-14)

## Итог и scope
- Session 10C была завершена без миграции startup/settings callsites; последующая Session 10D выполнила production migration и добавила необходимые player/app completion boundaries, описанные ниже.
- Новый `app-egui::media_open` — policy-neutral mechanism owner. Он не знает Item ID, queue reservation token, navigation, repeat/shuffle, manual/automatic priority или confirmation policy.
- `PlaylistRuntime` владеет одним coordinator на весь process lifetime и на resume привязывает clone существующего ordered `PlayerCommandSender`.

## Coordinator boundaries
- Opaque `MediaOpenClientKey` и `MediaOpenRequestId` коррелируют caller и request; exact command API различает no-current, stale request, invalid phase, missing binding и downstream backpressure/disconnect.
- Typed phases сохраняют `Accepted`, `Preparing`, `Prepared`, `PlayerStaging`, `ReadyToCommit`, `AuthorizationDispatchPending`, `EnqueuedAtPlayerOwner`, `Installed` и `Failed`.
- Caller явно выбирает coalesce/supersede/cancel/authorize. Ready pass-through никогда не auto-authorize-ится.
- Stage/authorize/cancel идут через D39/D52 ports существующего player owner-а. Authorization dispatch выполняется сразу в единый ordered player stream без второго buffer-а: успешный enqueue даёт `EnqueuedAtPlayerOwner`, Full/Disconnected остаются pre-enqueue rejection.
- После `EnqueuedAtPlayerOwner` cancel/Stop/suspend возвращают `CommitMustFinish`; delayed ack не разрешает abort. Missing control resolution, missing Installed, mismatched request и lifecycle cancel без authoritative resolution — fatal invariant.
- Повторный cancel во время pending control не отправляет вторую command и не заменяет request-owned receipt. Terminal забирается exactly once только с matching request ID.
- D52 update forwarding сохраняет exact player request/revision/intent и не имеет uncorrelated fallback.

## Bounded work и wake
- `executor.rs` владеет одним blocking preparation worker-ом и capacity-one latest pending slot; named budget `MAX_NON_CANCELLABLE_STALE_PREPARATIONS = 1`.
- Supersede делает running blocking open cooperative-stale, заменяет только latest pending work и никогда не возвращает stale result commit authority.
- OS thread spawn failure, executor/result/cancellation state loss и task panic не игнорируются: spawn возвращает typed start error, poison fail-closed, panic превращается в typed terminal.
- Worker публикует request-owned result slot до `AppWakePort::request_wake`; payload не переносится через winit.

## D64/D75 prepared envelope
- `PreparedLocalOpenResult` создаётся из одного `LocalFileSource` handle и одного `SymphoniaDemuxer::from_byte_source_with_options` open.
- До ownership transfer снимаются tracks, `LocalMediaKind`, duration, полный `MediaTagMetadata`, size+mtime fingerprint, safe label и reconstructible `ActiveMediaSource`.
- Cached fingerprint mismatch не запускает второй probe/open: фактически открытый envelope становится source of truth. Повторное изменение path fingerprint до transfer даёт typed `LocalSourceChanged` без retry loop.
- `playlist-discovery::classify_local_media_tracks` переиспользует общую topology vocabulary без I/O; explicit target не вызывает `probe_one_local_media`.
- Direct и YouTube adapters используют существующие service owners/typed locators. YouTube descriptor сохраняет exact `YoutubeSelectedStreamIdentity`; direct/YouTube formatting остаётся redacted.
- `ActiveMediaSource` теперь принадлежит reusable media-open vocabulary и re-export-ится прежним state module до Session 10D, чтобы не было второго reconstructible-source enum-а.

## Проверки и тесты
- Focused media-open: 17 PASS с default и no-default features. Покрыты single-open/mismatch/second-change, source parity/redaction, все cancellation causes, coalesce/supersede/stale request, bounded stale work, Ready/no-auto-authorize, enqueue/rejection/cancel winner, delayed ack+suspend, repeated cancel, missing resolution/Installed fatal, D52 forwarding и panic isolation.
- Neighbor suites PASS: 18 source-core, 52 playlist-discovery, 127 symphonia-demux, 9 direct (+1 ignored), 33 YouTube (+4 ignored), 17 staged player, 6 playback-intent.
- Strict touched-crate Clippy, fmt, Rust 1.96 locked workspace check, refactor guardrails и diff check PASS.
- Полный `app-egui --no-default-features`: 302 PASS / 1 unrelated pre-existing failure `startup_media::tests::cli_route_rejects_unsupported_media_protocol`; Session 10C не меняла этот route или `startup_media.rs`.
- Serena existing-file diagnostics чисты; новые linked `media_open` files временно получают stale rust-analyzer `unlinked-file` hint, тогда как Cargo build/test/check доказывают module linkage.


## Session 10D production migration (2026-07-14)
- `crates/app-egui/src/state/strong_media_open.rs` — единый owner-focused adapter startup/local/settings: caller-prepared media входит без повторного demux open, проходит Prepared -> PlayerStaging -> ReadyToCommit -> explicit authorize -> EnqueuedAtPlayerOwner -> Installed. Только exact Installed завершает success.
- `DetachedVideoBackendSelection` переносит player-selected backend ID + exact frame contract через `video-backend-api`; app candidate port создаёт matching renderer/materializer half и коммитит pointers только после Installed. Renderer recreation продвигает app renderer generation.
- Production callsites больше не вызывают `PlayerWorker::load_prepared_media` напрямую; facade остался один только для focused player tests и помечен TODO. Direct stage/authorize app calls ограничены `media_open/player_port.rs`.
- Exact settings restore использует request + `MediaInstanceId` для position/video/audio/subtitle tracks и D52 exact intent receipt. Post-barrier failure классифицируется как `PartialFailure`, поэтому settings transaction correlated reinstall rollback также ждёт Installed; pre-barrier failure остаётся `Failed`.
- Authorization backpressure остаётся pre-barrier rejection, после которого strong adapter использует lossless cancel delivery в том же ordered player stream. Missing owner resolution/Installed остаётся fatal.
- PASS: 309 app no-default, 516 player-core, 14 coordinator, 43 settings-runtime, strict app/player Clippy, fmt, Rust 1.96 locked workspace check, guardrails, diff check и Serena reference/diagnostic audit. Startup protocol-message regression исправлен secret-safe. Следующий scope — Session 11A.

## Session 14A admission boundary (2026-07-15)
- Coordinator остаётся policy-neutral и не видит unconfirmed in-app intent. `PlaylistRuntime`-owned D79 boundary находится выше него; matching Confirm атомарно consumes slot и только затем выдаёт admitted original intent в существующий preparation/strong-open route.
- Local picker больше не вызывает demux/path preparation: выбранный target возвращается runtime-у, а отдельный preparation job создаётся только после empty-queue admission либо Confirm. Trusted CLI/startup использует отдельный typed origin.
- Полный контракт: `mem:app-egui/queue-replacement-confirmation-s14a`.


## Session 14B suspend/resume continuation (2026-07-15)
- `PlaylistRuntime` uses the same coordinator for runtime-only active-media reopen. Suspend terminal-resolves pre-dispatch work and waits authoritative dispatch winner; enqueue winner drains exact Installed before checkpoint capture.
- Resume stages `StartPaused`, then exact seek/non-seekable resolution and stable intent occur outside coordinator before same-lineage controller rebind. Pre-Installed resume failures consume/cancel the terminal so explicit Retry never inherits a hidden Busy slot.
- Exact YouTube selected-stream identity can be reopened through the service adapter without silently reselecting another stream. Full ownership/order contract: `mem:app-egui/suspend-resume-checkpoint-s14b`.


## Session 17 nonblocking startup consumer (2026-07-16)
- Startup использует отдельный renderer-bound stepwise strong-install driver и не блокирует winit event loop; policy-neutral coordinator не получил CLI/fallback/queue policy. Blocking wrapper сохранён для прежних non-startup callers. Полный контракт: `mem:app-egui/startup-orchestration-s17`.

## Generic yt-dlp reconstruction update (2026-07-17)

- Все прежние app composition variants `YouTube` заменены на `YtDlp`: source request, prepared descriptor, active source, startup job, suspend checkpoint, settings rebuild и playlist metadata source.
- `YtDlpMediaLocator` хранит exact HTTP(S) identity и проходит через coordinator/reopen без повторного app parsing. Selected-stream reconstruction хранит `YtDlpSelectedStreamIdentity`; refresh/open должен совпасть с exact chosen candidate и не может молча выбрать другой stream.
- Coordinator остаётся policy-neutral: generic host admission, WebM/VP9/Opus compatibility, process args/errors и URL privacy принадлежат `service-ytdlp`; capability selection остаётся app/capability boundary; player получает только готовый `PreparedMedia`.
- URL registry выбирает direct-media до yt-dlp. После выбора adapter фиксирован, поэтому direct open failure не даёт hidden yt-dlp retry.
- Metadata enrichment не меняет playback/coordinator lifecycle и revalidates exact Item ID + locator; см. `mem:app-egui/ytdlp-playlist-metadata-2026-07-17`.
- Privacy/exact identity: `mem:media-services/secret-safe-locators-s10b`.


## S14 CUE integration (2026-07-20)
`PlaylistRuntime::media_open_intent_for_planned_install` теперь под одним exact queue revision/item guard возвращает physical locator и optional neutral `MediaPlaybackWindow`. App строит physical `MediaOpenSourceRequest` и только затем оборачивает его в `PlaybackWindow`, поэтому coordinator по-прежнему получает один source-neutral request, а CUE-типы не протекают в player-core. Полный контекст: `mem:app-egui/cue-integration-s14-2026-07-20`.


## S17S committed-playlist startup consumer (2026-07-21)
- Startup playlist не добавляет новый media-open coordinator или parser. После successful `StartupReplace` commit `AppState::begin_startup_playlist_install` materializes exact committed locator/window и входит в существующий stepwise strong-open protocol.
- Boundary принимает один prevalidated `PlannedPlaylistInstall`; queue revision и source-order first Item проверены runtime receipt-ом до source materialization. Synchronous source/strong rejection маркирует только этот Item failed и не вызывает normal transport queue, sibling fallback или sequential scan.
- Existing Ready authorization, EnqueuedAtPlayerOwner/Installed, cancel-win, fatal/post-barrier и startup retained-action semantics не изменены.


## S21W player-side staged continuation compatibility (2026-07-21)

- App coordinator protocol не изменился: receipt по-прежнему различает Accepted/ReadyToCommit/Installed/terminal failure, а authorization до Ready получает `NotReady`.
- Player worker теперь может удерживать staged request в resumable preflight до demux retry/timeout deadline; coordinator не должен busy-poll-ить, повторно отправлять stage request или считать отсутствие immediate Ready ошибкой.
- Supersede/cancel/shutdown сохраняют exact `MediaInstallRequestId`, прежние typed cancellation causes и exactly-once terminal response. Commit barrier и current-media preservation до authorization остались прежними.
