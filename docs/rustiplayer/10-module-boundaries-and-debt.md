# 10. Границы модулей и долг

Этот файл фиксирует места, где код уже работает, но границы ещё не полностью
разделены. Это не баг-лист для немедленного переписывания, а карта рисков.

## Текущие временные exceptions

После последних refactor PR прямые зависимости
`player-core -> symphonia-demux/webm-demux`, `player-core -> video-vaapi`,
`player-core -> audio`, `player-core -> wgpu` и обратная dependency от
`video-vaapi` к `player-core` закрыты. Долг старого mixed render crate тоже
закрыт: `render-wgpu` разделён на `render-wgpu-video` и `render-wgpu-shell`, а
reference backend `video-vulkan` удалён из workspace и Cargo graph.

Сейчас нет зафиксированных temporary dependency exceptions для старого
`render-wgpu -> video-vulkan` долга. Оставшиеся разделы ниже описывают
архитектурные зоны риска, а не allowlist для прямых Cargo dependency нарушений.

Это не делает контрактные crates backend-specific: `media-core`, `codec-core`,
`audio-core`, `video-core`, `video-backend-api`, `render-core` и
`capability-core` остаются нейтральными contract boundaries.

## `player-core::PlayerSession`

`PlayerSession` остаётся центральным owner-ом playback state machine, но его поля
закрыты от sibling modules. Даже внутренний `PlaybackPipeline` slot больше не
является `pub(crate)` полем session: внешние модули `player-core` должны идти
через session-owned boundary methods.

Устойчивые поддомены уже вынесены в `session/*`: media lifecycle, capability
selection, audio runtime, diagnostics sink, EOF drain, seek transaction,
snapshot builder, render lease boundary и `session/tick/*`. Session всё ещё
координирует:

- media open/install/reset;
- command dispatch;
- seek commit gates;
- audio/video scheduler helpers;
- capability refinement;
- render lease handoff/accounting;
- diagnostics aggregation.

Render bridge больше не читает `session.pipeline` напрямую: он получает stable
present-frame identity через `PlayerSession::current_present_frame_identity()` и
создаёт render lease через `PlayerSession::lease_present_video_frame()`. Это
оставляет ownership active decoder guard-а, render generation и texture handle
identity внутри session boundary.

Следующий безопасный шаг: уменьшать оставшийся orchestration surface только
там, где появляется новый устойчивый поддомен с тестами; не возвращать прямой
доступ к полям ради удобства.

Media opening уже вынесен из прямой зависимости `player-core -> symphonia-demux/webm-demux`:
`player-core` принимает `PreparedMedia`. Оставшаяся работа здесь не в том, чтобы
вернуть concrete opener внутрь session, а в том, чтобы уменьшить orchestration
surface самого `PlayerSession`.

## `player-core::PlaybackPipeline`

`PlaybackPipeline` больше не является широким `pub(crate)` хранилищем runtime
slots. Struct остаётся crate-visible как внутренний владелец состояния
`player-core`, но его поля закрыты. Session/tick/snapshot code внутри session
boundary обращается к pipeline через intent methods, описанные в
[09. Контракты и Internal API](09-contracts-and-internal-api.md), а не через
конкретное устройство storage. Sibling modules вне `session` не должны читать
pipeline slot у `PlayerSession`.

Закрытые домены уже включают media source/demux, track selection, active video
requirement, seek generation, audio runtime/clock, packet queues, presentation
queues, video decoder I/O, in-flight packet accounting, render generation и
render lease accounting.

Оставшийся долг:

- surface area boundary methods всё ещё широкий, потому что `PlayerSession` и
  tick/scheduler пока разделяют orchestration внутри одного crate;
- `select_video_track_preserving_active_requirement()` является transitional
  legacy command path. TODO хранится рядом с методом: удалить его, когда выбор
  video track будет всегда передавать заново проверенный
  `VideoDecodeRequirement`;
- helper packet records `DecodedAudioPacket`, `PendingAudioPacket` и
  `PendingVideoPacket` всё ещё имеют `pub(crate)` поля. Это отдельный transport
  scope, не полевая граница `PlaybackPipeline`.

Следующий безопасный шаг: сужать не поля, а слишком крупные orchestration
домены и boundary method surface. Каждое сужение должно идти с focused tests на
absent resource, active fake/stub, typed error/no-op и accounting edge cases.

## Размер модулей

Большинство production modules после session decomposition укладываются в
ориентир до 2k строк. Временные исключения на момент cleanup-сессии:

- `crates/player-core/src/pipeline.rs` около 2.8k строк: широкий internal owner
  runtime slots; дальнейшее дробление должно идти только через устойчивые
  state domains, а не косметически.
- `crates/player-core/src/session/tick/mod.rs` около 2.2k строк: содержит public
  tick config/result types и основной `PlayerSession::tick`; child modules уже
  вынесены для demux admission, presentation scheduler, decoder I/O и wakeup.
- `crates/player-core/src/worker.rs` около 2.2k строк: worker runtime и тесты
  ещё плотные; это отдельный future boundary, не часть текущего cleanup.

## `app-egui::AppState::player_snapshot()`

Метод не является чистым getter-ом: он читает latest snapshot, обновляет cache и
публикует snapshot в desktop integration. Это практично для текущего shell, но
граница "read snapshot" и "publish desktop state" смешана.

Следующий шаг: разделить `refresh_player_snapshot()` и `publish_desktop_snapshot()`
или сделать явный per-frame shell context.

## `app-egui` shell decomposition

`main.rs` теперь остаётся процессной точкой входа: инициализирует tracing,
загружает config, разбирает CLI initial media, создаёт `EventLoop` и передаёт
управление `AppShell`. Shell-level orchestration больше не живёт в entrypoint-е.

Текущая shell boundary разделена по стабильным поддоменам:

- `app_shell` владеет `winit::ApplicationHandler`, окном, runtime
  restore/drop, renderer/app-state wiring и применением redraw decisions к
  `ControlFlow`/`request_redraw`;
- `render_settings` мапит валидированный `AppConfig` в renderer-neutral
  `render-core` настройки color pipeline и HDR-to-SDR;
- `system_capabilities` выполняет shell-level capability scan: VA-API provider
  плюс render capabilities;
- `startup_media` владеет CLI initial media, startup error и background
  подготовкой YouTube startup job;
- `redraw_pacing` владеет redraw pacing и timed polling decisions для shell
  background jobs.

Следующий безопасный шаг: сужать отдельные shell helpers только при появлении
нового устойчивого поддомена с focused tests; не переносить playback queues,
scheduling, demux state, decoder state или renderer/GPU internals обратно в
`main.rs` или UI widgets.

## Backend API и WGPU materialization boundary

`video-backend-api` владеет startup/resource-provider contract между playback и
concrete video backend-ами. В нём живут `VideoBackendFactory`,
`StartedVideoBackend`, `PresentFrameResourceProvider` и cloneable provider
handle для lookup/release decoded resources.

`player-core` зависит от `video-backend-api` и `video-core`, принимает
`StartedVideoBackend`, ведёт render lease accounting и различает absent resource,
busy texture pool, missing handle, fatal backend error и нормальный release path.
Он не зависит от `video-vaapi`, не зависит от `wgpu` и не возвращает
`wgpu::TextureView`.

`video-vaapi` реализует `VideoBackendFactory` из `video-backend-api` и владеет
VA-API decode thread-ом, DMA-BUF/WGPU import-ом и texture pool lifetime.
Concrete backend crates не должны зависеть от `player-core`; adapter boundary
идёт через `video-backend-api`.

WGPU materialization остаётся в `app-egui`/`render-wgpu-video`: shell получает
`VideoTextureViewProvider` из `VaapiWgpuVideoBackendFactory::start_for_composition()`,
создаёт `WgpuFrameTextureViewMaterializer` и передаёт renderer-у WGPU texture
views без переноса GPU handles в `player-core`. `render-wgpu-shell` отвечает за
WGPU surface/device lifecycle, egui composition и present, но не владеет decoded
video resources.

## `service-youtube`

`service-youtube` теперь умеет отдавать capability-aware stream candidates из
manifest metadata, но основной startup/playback path всё ещё использует старый
SDR-safe selector и возвращает уже открытый demuxer для совместимости. Поэтому
интеграция `capability-core` в реальный YouTube startup остаётся отдельным
архитектурным шагом.

Следующий шаг: service candidates -> capability selection -> demux open без
изменения HTTP refresh/range boundary.

## `symphonia-demux`

`symphonia-demux` владеет concrete adapter-ом поверх Symphonia и открывает
audio containers, которые поддерживает текущая Cargo feature set Symphonia. Старый
crate `webm-demux` оставлен только как compatibility re-export на transition PR,
чтобы внешние call sites могли мигрировать без одновременного изменения
поведения demux/seek/decode.

Миграция Symphonia закрыла активный долг локального fork-а: workspace dependency
теперь идёт в upstream `symphonia = 0.6`, а устаревшие локальные каталоги патчей
Symphonia удалены из workspace. Demux/audio path больше не имеет локального
Symphonia fork как source-level или Cargo-level fallback.

`infer_track_kind()` пока считает всё non-audio видео. Unknown video codec уже не
маскируется под VP9, но определение kind всё ещё грубое.

Следующий шаг: опираться на container metadata там, где Symphonia/Matroska
pre-scan даёт явный TrackType.

## `render-wgpu-video` и `render-wgpu-shell`

NV12/P010 video renderer теперь живёт в `render-wgpu-video`, а WGPU
instance/device/surface lifecycle и egui composition живут в
`render-wgpu-shell`. Это закрывает старый долг mixed crate и reference
dependency на `video-vulkan`.

Следующий безопасный шаг для renderer layer-а: удерживать `render-core` как
нейтральный contract и не переносить туда WGPU-specific details. Если появится
второй renderer backend, он должен подключаться через новые boundary methods и
focused tests, а не через восстановление старого mixed `render-wgpu` crate.

## Patched dependencies

`cros-codecs` и `cros-libva` patched локально. Symphonia patch закрыт полностью:
активная dependency идёт через upstream `symphonia = 0.6`, локальный fork удалён,
и это больше не является source-level долгом между demux/audio и VA-API/WGPU.

Следующий шаг: периодически проверять upstream и удалять patches только после
прохождения zero-copy/HDR regression matrix.
