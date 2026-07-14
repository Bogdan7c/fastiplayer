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