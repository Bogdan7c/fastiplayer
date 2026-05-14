# 03. Project Map

## Текущее состояние

Сейчас workspace уже разделен на несколько полезных crate'ов:

```text
crates/app-egui         - окно, egui, render loop, CLI/file/url shell boundary
crates/player-core      - PlayerWorker, PlayerSession, commands/events/snapshot, A/V scheduler
crates/media-core       - Track/Packet/media metadata types
crates/codec-core       - typed codec/profile/color/frame contracts и codec adapter registry
crates/capability-core  - capability report и stream selection reasons
crates/service-youtube  - временный yt-dlp adapter и HTTP streaming boundary
crates/audio            - Opus decode, CPAL output, audio clock
crates/render-core      - контракты renderer capabilities и renderable frame
crates/render-wgpu      - WGPU surface/egui composition, NV12 SDR renderer и Phase 10 P010/HDR BT.2446-C renderer
crates/video-core       - базовые video types и decoded frame contract
crates/video-vaapi      - VA-API hardware decode thread, capability probe и texture cache
crates/video-vulkan     - reference Vulkan Video код
crates/webm-demux       - Symphonia-based WebM/Matroska demux
crates/vp9-parser       - VP9 parser adapter
crates/config           - TOML config schema/load/create
crates/cros-codecs-patch - локальный patched `cros-codecs` dependency для текущего VP9/VA-API path
crates/cros-libva-patch - локальный patched `libva` wrapper dependency для DMA-BUF/VA-API interop
```

Главный оставшийся долг: `app-egui` всё ещё является desktop shell с несколькими runtime wiring helpers, но codec/capability/player decisions уже вынесены из UI-слоя. После live seek/timeline Session 3 `app-egui` больше не владеет media pipeline: `PlayerWorker` держит `PlayerSession`, выполняет tick на отдельном потоке, отдаёт shell-у latest snapshot/event stream и выдаёт decoded frame только через render lease. `app-egui` получает `wgpu` texture views на render thread через provider lease-а и не читает `pipeline.present_video_frame` напрямую.

## Целевая карта crate'ов

```text
crates/
  app-egui/
  player-core/
  media-core/
  codec-core/
  capability-core/
  source-core/
  service-youtube/
  demux-webm/
  demux-mp4/
  demux-mpeg-ts/
  audio/
  video-core/
  video-backend-vaapi/
  video-backend-dx12/
  video-backend-videotoolbox/
  render-core/
  render-wgpu/
  render-gles/
  config/
  desktop-integration/
  telemetry/
  test-matrix/
```

Не все crate'ы создаются сразу. Карта нужна, чтобы не смешивать ответственность при переносе MVP.

## Crate responsibilities

### `app-egui`

Desktop shell.

Отвечает за:

- winit lifecycle;
- создание окна;
- egui input/output;
- UI layout;
- dispatch `PlayerCommand` через `PlayerWorker`;
- отображение `PlayerSnapshot`;
- вызов render backend;
- запрос `PresentFrameLease` через worker render lease boundary;
- получение `wgpu::TextureView` на render thread через render-side provider lease-а;
- отправка typed `PlayerRenderError` в worker при render bridge/render failure;
- показ user-facing errors.

Не отвечает за:

- demux;
- decode;
- A/V sync;
- source extraction;
- capability probing.
- прямой доступ к `PlayerSession`, `PlaybackPipeline` или `pipeline.present_video_frame`.

### `player-core`

Центральная state machine плеера.

Отвечает за:

- `PlayerWorker`;
- `PlayerCommandSender`;
- `PlayerSession`;
- `PlayerCommand`;
- `PlayerEvent`;
- `PlayerSnapshot`;
- `PlayerWorkerEvent`;
- `PresentFrameLease` / compatibility alias `PlayerPresentFrame`;
- `PlayerRenderError`;
- `SeekController`;
- playback states;
- play/pause/seek/scrub/stop/drain/EOF;
- orchestration source -> demux -> audio/video -> scheduler;
- queue/backpressure policy;
- error policy.

`PlayerWorker` является текущей runtime boundary: он использует `crossbeam-channel`
для bounded command queue, отдельного bounded latest scrub channel, latest snapshot
publisher, event stream, render frame lease request channel, render release ack
channel и shutdown signal. `UpdateScrub` coalescing реализован политикой
`Drain Latest`, чтобы high-rate drag events не раздували общую очередь.
`PlayerSession::tick()` остаётся внутренним API `player-core`, но вызывается
worker-потоком, а не `app-egui`.

Render bridge в `player-core` отдаёт shell-у `PresentFrameLease` с frame metadata,
texture handle, render generation и stale flag. Worker выбирает handle из
worker-owned pipeline, но не создаёт `wgpu::TextureView`. Release texture slot-а
идёт через shared RAII drop/ack; typed render bridge failures попадают обратно в
worker как `PlayerRenderError` и обновляют player error snapshot.

`player-core` хранит seek/timeline state в `PlayerSnapshot` через typed
`TimelineSnapshot`, а legacy `duration/current_position: Duration` остаются
compatibility-полями для текущего UI до отдельной UI-сессии. Контракты
`SeekTarget::Absolute(MediaTime)`, `SeekTarget::Relative(Duration)`,
`BeginScrub`, `UpdateScrub`, `PreviewScrub`, `EndScrub { CommitLatest }` и
`Stop` не должны содержать backend/container/platform-specific details.

### `media-core`

Общие media-типы без привязки к контейнеру и backend.

Отвечает за:

- `TrackId`;
- `TrackInfo`;
- `Packet`;
- `MediaTime`;
- `MediaDuration`;
- `TrackTimestamp`;
- `TimelineRange`;
- `TimelineSnapshot`;
- `TimeBase`;
- `MediaInfo`;
- `ContainerKind`;
- `StreamDescriptor`;
- subtitle descriptors;
- audio/video track metadata;
- optional video coded/display width and height, если контейнер или manifest сообщает их до decode.

Текущие media-типы уже живут в `media-core`; `webm-demux` использует их как общий contract, а не владеет player-facing packet model.

Timeline-типы в `media-core` являются core-neutral. Они описывают media time,
duration, track timestamp conversion и seekable window, но не знают о WebM,
Matroska, DASH, YouTube, VP9, VA-API, wgpu, MPRIS или конкретном HTTP cache.
Конкретные первые adapters только заполняют эти типы из своих metadata.

### `codec-core`

Типизированная модель codec/profile/capability и codec adapter boundary.

Отвечает за:

- `VideoCodec`;
- `AudioCodec`;
- `VideoProfile`;
- `CodecLevel`;
- `BitDepth`;
- `ChromaSubsampling`;
- `VideoSurfaceFormat`;
- `VideoMemoryContract`;
- `ZeroCopyExportRequirement`;
- `ColorPipelineRequirement`;
- `FrameTimingContract`;
- `HdrMetadata`;
- `ColorRange`;
- `MatrixCoefficients`;
- `ColorPrimaries`;
- `TransferFunction`;
- `VideoColorMetadata`;
- `ColorMetadataOrigin`;
- `ColorMetadataConfidence`;
- `VideoRequirementRejection`;
- `ColorMetadataConflict`;
- `SupportedDecodeFormat`;
- normalization codec ids из контейнеров и сервисов;
- generic `VideoMetadataSource` -> `VideoDecodeRequirement` resolution;
- codec adapter registry для packet/header probing;
- текущий VP9 adapter как первый concrete adapter над `vp9-parser`.

Не отвечает за:

- прямое чтение VA-API;
- запуск decode backend;
- ad-hoc bitstream parsing в `player-core`, demuxer-е, renderer-е или UI.

Если capability selection требует profile/bit-depth/chroma/resolution из bitstream, `codec-core` хранит typed модель и adapter API. Сам parser должен быть codec adapter-ом над уже используемым parser'ом: VP9 сейчас использует `vp9-parser`, будущие AV1/H.264/H.265 должны использовать parser из decode backend или тонкий adapter над ним. `player-core` только вызывает generic adapter functions и получает typed `VideoDecodeRequirement`/reject/recoverable uncertainty.

Zero-copy выражается не именем backend-а, а `VideoMemoryContract::HardwareZeroCopy { export: ZeroCopyExportRequirement::DmaBuf }`. Новый codec не может включить CPU fallback через отдельный branch: capability intersection обязана удовлетворить общий memory contract, surface format и renderer import capability.

Color metadata в `codec-core` описывает факты о потоке, а не пользовательские настройки изображения. Defaults вроде `sdr_bt709_limited()` должны быть явными helper-ами, чтобы fallback не выглядел как metadata, полученная из bitstream.

### `capability-core`

Сводная информация о системе.

Отвечает за:

- запуск backend probes;
- объединение результатов VA-API/render/platform;
- построение capability matrix;
- выбор лучшего playable stream;
- сериализацию capability report для UI и диагностики.

### `source-core`

Источник bytes и manifests.

Отвечает за:

- local file source;
- HTTP/range source;
- cached source;
- stream readers;
- credential/session boundary;
- общий contract для online services.

### `service-youtube`

Service boundary для YouTube.

Текущее состояние: временный `yt-dlp` adapter живёт здесь, а не в `app-egui`.
Он по умолчанию выбирает текущий поддержанный SDR VP9/Opus WebM selector, получает direct media URLs/headers и отдаёт player shell-у готовый streaming demuxer. Phase 10 local HDR/P010 support не меняет default YouTube selector: HDR/VP9.2 проверки идут через `VIDEO_PLAYER_YOUTUBE_FORMAT_SELECTOR` до появления capability-aware service candidates. Полная Rust-замена extractor-а остаётся будущей задачей этого же crate-а.

Отвечает за:

- временную интеграцию `yt-dlp`;
- YouTube extraction;
- account/session/cookies;
- manifest parsing;
- stream selection metadata;
- captions;
- live stream metadata;
- сервисные ошибки и rate-limit handling.

### `demux-webm`

WebM/Matroska demuxer.

Сейчас функциональность живет в `webm-demux`. Позже crate можно переименовать или оставить, но player-facing media-типы уже вынесены в `media-core`.

Текущий seek contract: `DemuxSeekRequest` содержит target time и режим. Для video
`player-core` использует `DecodePointBefore`, а `webm-demux` мапит его на
Symphonia/Matroska coarse seek к decode-safe точке не позже target. Точный commit
остаётся в `PlayerSession` через decoder reset, pre-roll/drop и commit gates.

### `demux-mp4`

MP4/MOV/fMP4 demuxer.

Будущий crate для:

- MP4;
- MOV;
- fragmented MP4;
- DASH segments.

### `demux-mpeg-ts`

MPEG-TS/HLS segments.

Будущий crate для:

- TS packets;
- HLS media segments;
- timestamp normalization.

### `audio`

Software audio pipeline.

Отвечает за:

- audio decode;
- resampling;
- channel layout conversion;
- audio buffer;
- CPAL output;
- audio clock.

Текущее состояние после worker/audio стабилизации: `AudioClock` использует CPAL
playback/callback timestamps как playback anchors, хранит previous/latest anchor
и интерполирует media sample index между output callbacks. Linear resampler хранит
carry frame между packet boundaries, чтобы не создавать слышимые разрывы при
ресемплинге.

Со временем crate может быть разделен на `audio-core`, `audio-codecs`, `audio-output-cpal`, если станет слишком большим.

### `video-core`

Общие video decode/render bridge-типы.

Отвечает за:

- decoded frame descriptor;
- frame handles;
- color metadata;
- decoded surface format через compatibility alias на `codec-core::VideoSurfaceFormat`;
- bit depth/chroma metadata;
- decoded memory path; production path допускает только `DmaBufZeroCopy`, `CpuUpload` остаётся test-only negative coverage marker;
- video frame lifecycle contracts;
- backend-independent decode traits.

Не должен знать о VA-API, DX12, VideoToolbox или egui.

### `video-backend-vaapi`

Linux hardware decode backend.

Сейчас это `video-vaapi`. Целевое имя можно выбрать позже. Ответственность:

- VA-API display open;
- i965/iHD probing;
- codec/profile capability scan;
- backend-specific capability mapping в общие `SupportedVideoDecodeFormat`/surface/memory contracts;
- hardware decode;
- frame pool;
- DMA-BUF/texture upload integration;
- render-side `VideoTextureViewProvider` для получения Y/UV `wgpu::TextureView`
  по opaque frame handle без доступа к player pipeline;
- errors with driver/backend context.

### `video-backend-dx12`

Future Windows backend.

Пока только reserved architecture.

### `video-backend-videotoolbox`

Future macOS backend.

Пока только reserved architecture.

### `render-core`

Renderer contracts.

Отвечает за:

- `RenderBackend`;
- `RenderCapabilities`;
- `RenderableFrame`;
- color conversion contract;
- tone mapping contract;
- color pipeline settings contract;
- active color path diagnostics;
- swapchain transfer mode contract;
- UI composition contract;
- present timing diagnostics.

### `render-wgpu`

Primary renderer.

Отвечает за:

- wgpu instance/adapter/device/surface;
- Vulkan-first Linux path;
- DX12 path later;
- Metal path later;
- NV12 production shader;
- P010/HDR shader path из Phase 10;
- mapping typed color metadata to GPU uniforms;
- SDR color adjustments in shader uniforms;
- HDR-to-SDR tone mapping из Phase 10;
- distinction between P010 zero-copy boundary readiness and production P010 renderability;
- принятие `WgpuRenderableFrame`, собранного shell-ом из lease metadata и
  render-thread texture views;
- egui composition.

Текущее состояние: production NV12 path уже живёт в `render-wgpu`; Phase 10 добавил production P010/HDR renderer через отдельный `p010_bt2446c_to_sdr.wgsl` path. P010 renderability всё ещё зависит от фактических `wgpu` feature gates и DMA-BUF layout: Intel/i965 separate-layer `R16Unorm + Rg16Unorm` plane views являются baseline, composed `TextureFormat::P010` остаётся compatibility layout.

### `render-gles`

Future legacy renderer.

Отвечает только за:

- X11 fallback;
- OpenGL ES 2.0 compatible rendering;
- SDR 8-bit NV12;
- простая графика и минимальный UI composition path.

Не обязан поддерживать advanced UI/HDR parity.

### `config`

TOML settings.

Отвечает за:

- paths under `~/.config/rustiplayer`;
- `config.toml`;
- schema version;
- defaults;
- validation;
- migration between config schema versions.

### `desktop-integration`

Linux desktop integration.

Отвечает за:

- MPRIS D-Bus;
- KDE media widget integration;
- inhibit sleep/screensaver;
- notifications;
- future file associations.

### `telemetry`

Runtime metrics.

Текущая телеметрия живет в `app-egui`. Ее лучше вынести в отдельный crate, потому что metrics нужны player core, renderer, backend probes и UI.

### `test-matrix`

Тестовые manifests и helpers.

Отвечает за:

- каталог sample assets;
- описание codec/profile/HDR/FPS expectations;
- capability-based skip rules;
- regression tests для parser/demux/scheduler;
- golden samples для codec headers: VP9 uncompressed header, AV1 sequence header OBU, H.264 SPS, H.265 VPS/SPS.

## Правило зависимостей

Зависимости должны идти сверху вниз:

```text
app-egui
  -> player-core
  -> service-youtube
  -> render-core/render-wgpu
  -> desktop-integration

player-core
  -> media-core
  -> codec-core
  -> source-core
  -> audio
  -> video-core
  -> capability-core

backend crates
  -> codec-core
  -> media-core
  -> video-core
  -> render-core only when needed for frame interop
```

Запрещенные направления:

- `player-core -> app-egui`;
- `media-core -> app-egui`;
- `codec-core -> VA-API backend`;
- `service-youtube -> app-egui`;
- persistent database code -> playback/seek/scrub/source-cache hot path.
