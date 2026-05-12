# 12. Live Seek, Timeline and Desktop Controls Sessions

Этот документ разбивает реализацию live seek, timeline, cache/index и desktop controls
на отдельные рабочие сессии. Каждую сессию можно копировать в новый чат как
самостоятельное задание: внутри есть контекст, границы, тесты, ручные проверки и
обязательный self-review.

## Общий контекст

Цель: добавить взрослую перемотку и отображение времени без превращения UI в player.

Главные архитектурные решения:

- UI не хранит business state плеера. Он читает `PlayerSnapshot` и отправляет `PlayerCommand`.
- Seek/timeline core не привязан к container, codec, renderer или desktop platform.
- Первый production path реализуется адаптерами: WebM/Matroska, YouTube VOD WebM,
  VP9/Opus, VA-API, wgpu, Linux MPRIS.
- Video decode остаётся hardware-only. Software video fallback не добавляется.
- Playback живёт в worker thread и использует один основной zero-copy pipeline.
- Live scrub работает через single playback pipeline, а не через второй preview decoder.
- During scrub audio временно muted/paused; `EndScrub` коммитит последнюю позицию.
- Final commit точный в рамках stream granularity: seek before target, pre-roll/drop
  до первого video frame `>= target`, audio trim/reset до target или typed timeout/error.
- Network/cache/index настройки не хардкодятся, а идут через TOML config schema.
- Все публичные типы, контракты и сложные блоки документируются на русском.

Context7 basis, который нужно учитывать при реализации:

- Symphonia seek: `FormatReader::seek(SeekMode, SeekTo::Time { time, track_id })`;
  после seek decoder state должен быть сброшен.
- egui interaction: `Slider`/`Response` дают `changed`, `drag_started`,
  `drag_stopped`; UI должен отправлять команды, а не менять player state напрямую.
- reqwest Range: HTTP Range делается через headers; seekable HTTP source подтверждается
  реальным `206 Partial Content`, а не только `Accept-Ranges`.
- zbus: Linux MPRIS backend можно сделать через `zbus` blocking API в отдельном
  integration thread без глобального async runtime.

## Общие правила для всех сессий

- Перед правками читать релевантные текущие файлы, не писать по памяти.
- Не менять unrelated код и не откатывать чужие изменения.
- Не встраивать playback logic в `app-egui`.
- Не добавлять ad-hoc codec/container parsing в `player-core`.
- Любой новый config field получает default, validation и документацию.
- Каждый PR/сессия должен оставлять проект в runnable/checkable состоянии.
- Если для реализации нужно важное решение, которого нет в этой сессии, остановиться
  и уточнить решение до кода.

## Сессия 1. Contracts, Config and Architecture Docs

Статус в текущем коде: реализованы neutral timeline-типы в `media-core`,
расширены command/snapshot contracts `player-core`, добавлена config schema v2 и
обновлены архитектурные документы. На момент закрытия этой сессии playback worker
оставался следующим этапом; актуальный статус: worker реализован в Сессии 2, а
real demux seek и точное seek transaction behavior остаются scope следующих сессий.

### Контекст для копипаста

Реализуй первую сессию live seek/timeline плана. Нужны только контракты, config
schema и документация. Нельзя переносить playback в worker и нельзя менять runtime
поведение seek в этой сессии.

### Цель

Заложить нейтральные типы времени/timeline, команды seek/scrub, config defaults и
архитектурные документы так, чтобы следующие сессии не принимали новых решений.

### Scope

- Добавить в `media-core` typed timeline модель:
  - `MediaTime`
  - `MediaDuration`
  - `TrackTimestamp`
  - `TimelineRange`
  - `TimelineSnapshot`
- Обновить `player-core` command/snapshot contracts:
  - `SeekTarget::Absolute(MediaTime)`
  - `SeekTarget::Relative(Duration)`
  - `BeginScrub`
  - `UpdateScrub(SeekRequest)`
  - `EndScrub { policy: CommitLatest }`
  - `Stop`
- Добавить typed seek/timeline state в snapshot:
  - current timeline position
  - target position during seek/scrub
  - duration
  - seekable flag
  - not-seekable reason
  - seeking/scrubbing flags
  - stale frame flag
- Добавить config schema version 2.
- Добавить `player.seek.*`, `network.*`, `ui.skin`.
- Обновить docs: target architecture, project map, rendering/UI/platform, services/network,
  config/storage.

### Config defaults

- `player.seek.live_interval_ms = 100`
- `player.seek.live_preview_budget_ms = 100`
- `player.seek.commit_timeout_ms = 10000`
- `player.seek.resume_audio_min_buffer_ms = 50`
- `player.seek.paused_commit_behavior = "stay_paused"`
- `player.seek.hotkey_small_step_secs = 5`
- `player.seek.hotkey_large_step_secs = 30`
- `network.memory_cache_mb = 128`
- `network.read_ahead_mb = 64`
- `network.connect_timeout_ms = 15000`
- `network.read_timeout_ms = 15000`
- `network.indexer_io_budget_mb_per_sec = 32`
- `ui.skin = "minimal"`

### Validation

- `network.memory_cache_mb = 0` disables RAM cache.
- `network.memory_cache_mb <= 4096`.
- Timeouts must be positive.
- Seek intervals/budgets/timeouts must be positive.
- Hotkey steps must be positive.
- Unknown skin id is config error unless explicitly mapped to default by validation.

### Tests

- Unit tests for `MediaTime` conversion, ordering, saturating clamp.
- Unit tests for `TrackTimestamp` and timebase conversion.
- Config deserialization tests for schema version 2 defaults.
- Config validation tests for invalid timeout/cache/skin values.
- Player command/snapshot compile tests through existing `player-core` tests.

### Ручная проверка

- `cargo test -p media-core -p rustiplayer-config -p player-core`
- `cargo check`
- Open generated default TOML and confirm all new fields are visible and documented.

### Self-review

- Проверить, что `media-core` не импортирует player/render/source/backend crates.
- Проверить, что `player-core` contracts не упоминают WebM, VP9, VA-API, wgpu, MPRIS.
- Проверить, что новых magic constants нет вне config defaults/validation.
- Проверить, что старые `duration/current_position` либо совместимо мапятся, либо
  заменены без ломания текущих tests.
- Проверить, что документация явно говорит: core neutral, first adapters concrete.

## Сессия 2. Playback Worker and Seek State Machine Skeleton

Статус в текущем коде: выполнено, 2026-05-12.

Фактически реализовано:

- `player-core::PlayerWorker` владеет `PlayerSession` и media pipeline на отдельном
  потоке.
- `app-egui::AppState` владеет `PlayerWorker`, а не `PlayerSession`; UI/render loop
  больше не вызывает `player_session.tick()` напрямую.
- Worker boundary использует `crossbeam-channel`: bounded command queue,
  отдельный bounded latest channel для `UpdateScrub`, latest snapshot publisher,
  event stream, render-frame request/reply channel и shutdown signal.
- `PlayerCommandSender` применяет `Drain Latest` coalescing для `UpdateScrub`, чтобы
  drag events не накапливались без ограничения в общей очереди.
- `SeekController` skeleton содержит generation id, current mode, latest scrub
  target, in-flight target, resume intent и diagnostics counters для stale/ignored
  и cancelled операций.
- Command priority реализован на worker boundary: Stop/Open/Shutdown прерывают
  scrub, внешний `Seek` во время active scrub игнорируется, Play/Pause/Toggle
  обновляют resume intent, Volume/Mute идут как обычные immediate commands.
- Stop во время scrub отправляет pause + seek zero перед финальным Stop, что
  зафиксировано unit test-ом.

Проверки после реализации:

- `cargo test -p player-core -p app-egui` проходил после worker/refactor правок.
- `cargo test -p audio` проходил после audio clock/resampler стабилизации.
- `cargo check`, `cargo fmt --check` и `git diff --check` проходили после серии
  worker/audio исправлений.

Дополнительные заметки после ручной проверки:

- Первичная worker-миграция уменьшила UI/video flicker, но выявила audio crackle на
  тяжёлых 4k60 assets.
- Audio crackle был устранён не изменением worker architecture, а исправлением
  причины в audio path: CPAL playback timestamp теперь используется как
  previous/latest playback anchor, а interpolation умеет корректно работать до
  future anchor start; linear resampler сохраняет carry frame между packet
  boundaries.
- После audio fix основной остаточный симптом на тяжёлых 4k60 assets - late video
  drops. Текущая диагностика указывает на render/present cadence и late-drop
  scheduler policy под высокой нагрузкой. Эта проблема намеренно отложена в
  отдельный follow-up и не закрывается в Session 2.

### Контекст для копипаста

Реализуй playback worker. Эта сессия переносит владение `PlayerSession`/pipeline из
UI loop в worker thread и сразу готовит tick под seek modes, но ещё не обязана делать
реальный demux seek.

### Цель

Убрать блокирующую playback работу из render/UI thread и подготовить command/snapshot
модель для live scrub.

### Scope

- Добавить worker boundary в `player-core` или соседний module:
  - command sender
  - latest snapshot publisher
  - event stream
  - shutdown path
- Использовать `crossbeam-channel`.
- Worker owns `PlayerSession`.
- `app-egui` больше не вызывает `player_session.tick()` напрямую.
- Добавить `SeekController` skeleton:
  - generation id
  - current mode
  - latest scrub target
  - in-flight target
  - resume intent
  - stale/cancelled diagnostics counters
- Реализовать coalescing policy `Drain Latest` для `UpdateScrub`.
- Реализовать command priority:
  - Stop/Open/Shutdown interrupt scrub
  - external seek ignored during active scrub
  - Play/Pause during scrub updates resume intent
  - Volume/Mute apply immediately

### Tests

- Worker starts, accepts commands, publishes snapshot, shuts down cleanly.
- Command ordering for Play/Pause/Stop/Open/Shutdown.
- `UpdateScrub` coalesces to latest target.
- External `Seek` ignored during active scrub.
- Stop interrupts scrub and requests pause + seek zero.
- No command deadlock when receiver is closed.

### Ручная проверка

- Run app with local file and confirm current playback still works.
- Confirm UI remains responsive while playback worker is running.
- Quit app and confirm no hanging worker thread.
- `cargo test -p player-core -p app-egui`
- `cargo check`

### Self-review

- Проверить, что `app-egui` не владеет `PlayerSession`.
- Проверить, что worker shutdown deterministic and joined/dropped cleanly.
- Проверить, что command queue cannot grow unbounded with scrub updates.
- Проверить, что no locks are held while calling render/UI code.
- Проверить, что temporary dev flag не становится permanent public behavior.

## Сессия 3. Render Frame Lease

Статус в текущем коде: выполнено, 2026-05-12.

Фактически реализовано:

- `PlayerWorker::try_acquire_present_frame()` возвращает `PresentFrameLease`
  через compatibility alias `PlayerPresentFrame`, не раскрывая `PlayerSession`.
- Lease содержит decoded frame metadata, opaque texture handle, render generation
  и stale flag. `app-egui` дополнительно пересчитывает stale state по latest
  snapshot для cached lease-а.
- Worker выбирает текущий frame handle из worker-owned pipeline, но больше не
  создаёт `wgpu::TextureView` и не вызывает render-side `get_views`.
- Texture views создаются на render thread вызовом `PresentFrameLease::texture_views()`
  через `video-vaapi::VideoTextureViewProvider`; это view creation поверх уже
  импортированной/загруженной texture, а не копирование texture data.
- Shared RAII drop ack отправляется worker-у только при drop последнего clone-а.
  Если worker уже завершился и release channel disconnected, lease fail-closed
  освобождает texture через provider исходного кадра.
- Old generation lease может дожить после смены render generation: новый snapshot
  делает его stale, но inflight lease остаётся валидным до drop.
- Render bridge errors типизированы как `PlayerRenderError`/`PlayerRenderErrorKind`
  и попадают в worker через `WorkerCommand::RenderError`, публикуются как
  `PlayerWorkerEvent::RenderError` и обновляют `PlayerSnapshot.last_error`.
- `app-egui` не обращается напрямую к `pipeline.present_video_frame`; это закреплено
  regression test-ом.

Проверки после реализации:

- `cargo test -p player-core -p render-core -p app-egui` проходит.
- `cargo check` проходит.
- `cargo fmt --check` и `git diff --check` проходят.
- Ручной playback/missing-views smoke test остаётся manual verification пунктом,
  потому что требует локального video asset и GPU/runtime окружения.

### Контекст для копипаста

Реализуй render bridge после переноса playback в worker. Нельзя возвращать прямой
доступ UI/render к `PlayerSession` internals. Нужно сохранить zero-copy.

### Цель

Дать renderer-у безопасный доступ к текущему decoded frame через lease, не раскрывая
pipeline internals и не копируя texture data.

### Scope

- Добавить `PresentFrameLease`.
- Lease содержит:
  - frame handle
  - frame metadata
  - generation
  - stale flag
- Texture views получаются на render thread через render-side provider.
- Worker выбирает frame handle, но не создаёт wgpu views.
- Release через RAII drop/ack.
- Поддержать latest frame + one inflight lease.
- Render errors отправляются в worker typed command/event.

### Tests

- Lease drop releases frame exactly once.
- Leased old frame is not released while renderer still holds lease.
- New generation makes old frame stale but does not invalidate inflight lease.
- Render error command updates player error snapshot.
- No direct `app-egui` access to `pipeline.present_video_frame`.

### Ручная проверка

- Run video playback and confirm video still renders.
- Force or simulate missing texture views and confirm typed `PlayerRenderError`
  appears and updates player error snapshot.
- Confirm pause/resume does not leak texture handles.
- `cargo test -p player-core -p render-core -p app-egui`
- `cargo check`

### Self-review

- Проверить, что zero-copy path не заменён texture copy.
- Проверить, что worker не вызывает wgpu view access напрямую.
- Проверить, что release paths work on render failure, normal frame, shutdown.
- Проверить, что stale frame dimming state идёт через snapshot/lease metadata.

## Сессия 4. source-core and HTTP Range Source

Статус: реализовано

### Контекст для копипаста

Реализуй `source-core` и HTTP Range source. Нельзя встраивать HTTP/cache logic в
`webm-demux` или `service-youtube`. В этой сессии можно ещё не подключать YouTube seek.

### Цель

Создать нейтральный byte source слой для local/http seek, RAM cache и validators.

### Scope

- Добавить crate `source-core`.
- Ввести source contracts:
  - byte read
  - seek
  - seekability
  - validators
  - content length
  - source fingerprint
  - cancellation hook
- Реализовать local file source.
- Реализовать HTTP Range source через `reqwest::blocking`.
- Seekability probe требует `206 Partial Content`.
- Range failure retry once.
- Timeouts из config:
  - `connect_timeout_ms`
  - `read_timeout_ms`
- Реализовать RAM byte range cache:
  - global per media budget
  - `memory_cache_mb`
  - LRU/range-distance eviction
  - diagnostics hit/miss

### Tests

- Local source read/seek.
- HTTP local test server:
  - supports 206
  - returns 200 to range
  - timeout
  - interrupted response
  - retry once
- RAM cache hit/miss/eviction tests.
- `memory_cache_mb = 0` disables cache.
- Validation max cache size.

### Ручная проверка

- Run local HTTP test server manually and inspect logs for Range headers.
- Confirm non-range source reports seekable=false, not fatal.
- `cargo test -p source-core -p rustiplayer-config`
- `cargo check`

### Self-review

- Проверить, что `source-core` не знает про YouTube.
- Проверить, что `webm-demux` не содержит reqwest/service logic.
- Проверить, что cache budgets are from config only.
- Проверить, что all network errors are typed enough for player/UI.
- Проверить, что direct URL headers are passed as data, not hardcoded.

## Сессия 5. Local WebM/Matroska Seek Transaction

### Контекст для копипаста

Реализуй настоящий seek transaction для локальных WebM/Matroska. Используй
Symphonia seek через demux adapter. UI timeline можно пока оставить минимальным
или dev-only; главная цель - core transaction.

### Цель

Сделать точный commit seek для локального seekable media через single playback pipeline.

### Scope

- Обновить `webm-demux::Demuxer::seek` contract.
- `SymphoniaDemuxer::seek` использует `SeekTo::Time`.
- Seek track preference: video track if present, otherwise audio track.
- Convert `SeekedTo` actual timestamp to timeline info.
- Transaction steps:
  - stop normal demux read
  - increment generation
  - flush video decoder
  - clear pending audio/video packets
  - keep current frame as stale until new frame is ready
  - reset Opus decoder state
  - reset audio clock
  - request audio buffer clear with generation ack
  - demux seek
  - video pre-roll/drop to first frame `>= target`
  - audio trim/reset to target
- Commit timeout default `10000 ms`.
- Resume gate:
  - video frame ready
  - audio buffer at least `50 ms`
  - no-audio path waits for video frame only.

### Tests

- Fake demuxer seek transaction order.
- Generation rejects old packets/frames.
- Commit timeout yields pause + recoverable seek error.
- Paused-before-scrub stays paused by default.
- Playing-before-scrub resumes after gates.
- No-audio media seek resumes after video frame.
- Symphonia duration/time conversion unit tests.

### Ручная проверка

- Local WebM: click seek forward/backward.
- Pause, seek, confirm stays paused.
- Play, seek, confirm resumes after target frame/audio gate.
- Seek near beginning and near end.
- Confirm no old audio leaks after seek.
- `cargo test -p webm-demux -p audio -p player-core`
- `cargo check`

### Self-review

- Проверить, что exactness wording is stream-granularity based, not impossible promise.
- Проверить, что audio reset waits for callback ack, not sleep.
- Проверить, что old frame is only stale fallback, not marked as current target frame.
- Проверить, что demux errors, seek unavailable, timeout are separate typed errors.
- Проверить, что no UI code calls internal position mutation.

## Сессия 6. YouTube VOD Range Seek

### Контекст для копипаста

Подключи YouTube VOD seek через `source-core` HTTP Range source. Не добавляй
YouTube-specific logic в `player-core`. Live streams остаются not seekable.

### Цель

Сделать seek для YouTube VOD adaptive WebM streams через neutral source layer.

### Scope

- `service-youtube` отдаёт normalized direct stream descriptors:
  - URL
  - headers
  - format id
  - service media id
  - validators if available
  - duration if available
  - live flag
- Build video/audio `source-core` HTTP Range sources.
- `DualStreamDemuxer::seek` seeks both video and audio demuxers.
- Clear pending video/audio packets and EOF flags on seek.
- If direct URL expired, refresh once via service layer and retry range.
- If Range probe fails, mark source not seekable with reason.
- Live streams report not seekable.

### Tests

- Local HTTP server for dual video/audio range sources.
- `DualStreamDemuxer::seek` clears pending packet slots.
- URL expiry refresh once.
- Range unsupported disables seek, playback still possible.
- Missing validators means runtime index only, no persisted index.

### Ручная проверка

- Opt-in real YouTube smoke test via env.
- Seek forward/backward on YouTube VOD.
- Test expired URL path if reproducible or with local fake service.
- Confirm no required CI network dependency.
- `cargo test -p service-youtube -p source-core -p webm-demux -p player-core`
- `cargo check`

### Self-review

- Проверить, что `player-core` не содержит YouTube branch.
- Проверить, что service refresh is bounded to once.
- Проверить, что live streams are explicitly not seekable, not broken.
- Проверить, что HTTP headers from yt-dlp/service are preserved.
- Проверить, что real YouTube tests are env-gated only.

## Сессия 7. Minimal Timeline UI and Skin Boundary

### Контекст для копипаста

Реализуй минимальный player-style timeline UI. Поведение scrub уже должно идти через
commands/snapshot. UI не должен менять player position напрямую. Сразу заложи boundary
для будущей замены внешки, SVG и анимаций.

### Цель

Получить чистый minimal UI с live scrub и расширяемой skin архитектурой.

### Scope

- Выделить UI modules:
  - `ui/player_controls.rs`
  - `ui/timeline.rs`
  - `ui/skin/mod.rs`
  - `ui/skin/minimal.rs`
  - `ui/assets.rs`
  - `ui/animation.rs`
- Skin contract:
  - minimal skin first
  - `AssetProvider`
  - `IconId`
  - `AssetId`
  - `AnimationState`
- `ui.skin = "minimal"` selects first skin.
- Timeline behavior:
  - click = immediate seek
  - drag start = `BeginScrub`
  - drag move = `UpdateScrub`
  - drag end/focus loss/Escape = `EndScrub { CommitLatest }`
  - display target position during scrub
  - dim stale video frame
  - disabled timeline for not seekable source
- Time format:
  - `MM:SS`
  - `HH:MM:SS`
  - `--:--`

### Tests

- Timeline event mapper tests for click/drag/end.
- UI stores only transient pointer/drag value.
- Disabled timeline does not emit seek commands.
- Time formatter tests.
- Skin id config validation tests.

### Ручная проверка

- Local file playback with timeline click and drag.
- YouTube VOD timeline if session 6 is done.
- Not-seekable source shows disabled timeline.
- Stale frame dimming is visible during slow seek.
- Confirm controls do not overlap on small and normal window sizes.
- `cargo test -p app-egui -p player-core`
- `cargo check`

### Self-review

- Проверить, что `app-egui` не импортирует demux/audio/video internals for seek.
- Проверить, что future SVG/animation can be added via skin/assets boundary.
- Проверить, что UI text does not describe internals or shortcuts.
- Проверить, что no hardcoded behavior config lives in UI.
- Проверить, что telemetry metrics are not shown in normal controls.

## Сессия 8. Desktop Integration and MPRIS

### Контекст для копипаста

Реализуй platform-neutral `desktop-integration` crate и Linux MPRIS backend. Он должен
работать через worker command/snapshot boundary, а не напрямую через player internals.

### Цель

Добавить desktop media controls с seek support и не привязать core к Linux/D-Bus.

### Scope

- Добавить crate `desktop-integration`.
- Platform-neutral API:
  - command sink
  - latest snapshot source
  - desktop integration events/errors
- Linux backend:
  - `zbus` blocking thread
  - bus identity `org.mpris.MediaPlayer2.rustiplayer`
- MPRIS scope:
  - Play
  - Pause
  - PlayPause
  - Stop
  - Seek
  - SetPosition
  - Metadata
  - Duration
  - CanSeek
  - PlaybackStatus
- Stop = pause + seek zero, media stays open.
- Updates:
  - on property changes
  - `Seeked` after seek
  - no high-rate position spam
- Windows/macOS backends are stubs/future modules.

### Tests

- Command mapping tests: MPRIS methods -> `PlayerCommand`.
- Snapshot mapping tests: metadata/duration/can_seek/playback_status.
- Stop semantics test.
- Linux backend can be feature/cfg guarded.

### Ручная проверка

- On Linux desktop, check media widget sees Rustiplayer.
- Play/pause from desktop widget.
- Seek/SetPosition from MPRIS client if available.
- Stop returns to zero and pauses without closing media.
- `cargo test -p desktop-integration -p player-core`
- `cargo check`

### Self-review

- Проверить, что `player-core` не imports zbus.
- Проверить, что `app-egui` does not own MPRIS logic.
- Проверить, что D-Bus thread shutdown is clean.
- Проверить, что MPRIS position updates are not spammed.
- Проверить, что non-Linux builds have stubs or cfg gates.

## Сессия 9. Background Index and Cache Polish

### Контекст для копипаста

Реализуй фоновый keyframe/time index и polished diagnostics. Indexer не является
вторым playback pipeline: no decoder, no audio, no render, no texture pool.

### Цель

Улучшить random access/live scrub responsiveness без увеличения video decode resource usage.

### Scope

- Background indexer uses low-priority byte source/demux scan.
- Build metadata index:
  - track id
  - time
  - byte offset if available
  - keyframe flag
  - validator/fingerprint link
- Start with container cues.
- Improve index in background.
- Pause under:
  - active scrub
  - low playback buffer
  - high decode/render load
  - explicit shutdown
- Persist in SQLite through storage crate.
- Diagnostics:
  - index progress
  - cache hit/miss
  - range requests
  - seek latency
  - target vs actual
  - stale jobs
  - cancelled/superseded jobs
  - timeouts
- Metrics shown only in telemetry panel.

### Tests

- Index persistence and invalidation:
  - local size + mtime + partial hash
  - HTTP service id + format id + validators
  - missing validators => runtime only
- Indexer pauses under pressure.
- Cache/index diagnostics counters.
- SQLite migration tests.

### Ручная проверка

- Open local long file, scrub before and after index progress.
- Reopen same file, confirm persisted index is reused.
- Modify local file and confirm index invalidates.
- YouTube VOD with validators reuses eligible metadata where possible.
- `cargo test -p rustiplayer-storage -p source-core -p player-core`
- `cargo check`

### Self-review

- Проверить, что indexer does not decode video.
- Проверить, что indexer cannot starve playback network/disk reads.
- Проверить, что persisted offsets are never used without matching fingerprint/validators.
- Проверить, что diagnostics are useful but not visible in minimal controls.
- Проверить, что config controls all budgets and no magic IO constants remain.

## Финальная интеграционная проверка

После всех сессий:

- `cargo test`
- `cargo check`
- Local WebM:
  - play
  - pause
  - click seek
  - drag scrub
  - seek near start/end
  - stop
- YouTube VOD:
  - playback
  - range seek
  - drag scrub
  - URL refresh once path if testable
- Not-seekable source:
  - playback still works
  - timeline disabled with reason
- Desktop Linux:
  - MPRIS visible
  - Play/Pause/Stop/Seek/SetPosition
- UI:
  - minimal controls do not overlap
  - stale frame dim works
  - telemetry contains seek/cache/index metrics

## Финальный self-review

- Core contracts remain format/backend/platform neutral.
- First adapters are concrete but isolated.
- UI contains no demux/audio/video business logic.
- Playback worker owns player state.
- Single pipeline remains single: no second preview decoder.
- Zero-copy render path preserved through frame lease.
- All new limits are config-driven and documented.
- Errors are typed and user-facing only when actionable.
- Real YouTube tests are opt-in, not required CI.
- Architecture docs match implementation.
