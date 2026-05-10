# 03. Project Map

## Текущее состояние

Сейчас workspace уже разделен на несколько полезных crate'ов:

```text
crates/app-egui         - окно, egui, render loop, CLI/file/url shell boundary
crates/player-core      - PlayerSession, commands/events/snapshot, A/V scheduler
crates/media-core       - Track/Packet/media metadata types
crates/codec-core       - typed codec/profile/color/capability metadata
crates/capability-core  - capability report и stream selection reasons
crates/service-youtube  - временный yt-dlp adapter и HTTP streaming boundary
crates/audio            - Opus decode, CPAL output, audio clock
crates/render-core      - контракты renderer capabilities и renderable frame
crates/render-wgpu      - WGPU surface/egui composition и текущий NV12 renderer
crates/video-core       - базовые video types и decoded frame contract
crates/video-vaapi      - VA-API VP9 decode thread и texture cache
crates/video-vulkan     - reference Vulkan Video код
crates/webm-demux       - Symphonia-based WebM/Matroska demux
crates/vp9-parser       - VP9 parser adapter
crates/config           - TOML config schema/load/create
crates/storage          - SQLite migrations/history/progress/cache
```

Главный оставшийся долг: `app-egui` всё ещё является desktop shell с несколькими runtime wiring helpers, но codec/capability/player decisions уже вынесены из UI-слоя.

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
  storage/
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
- dispatch `PlayerCommand`;
- отображение `PlayerSnapshot`;
- вызов render backend;
- показ user-facing errors.

Не отвечает за:

- demux;
- decode;
- A/V sync;
- source extraction;
- storage schema;
- capability probing.

### `player-core`

Центральная state machine плеера.

Отвечает за:

- `PlayerSession`;
- `PlayerCommand`;
- `PlayerEvent`;
- `PlayerSnapshot`;
- playback states;
- play/pause/seek/drain/EOF;
- orchestration source -> demux -> audio/video -> scheduler;
- queue/backpressure policy;
- error policy.

### `media-core`

Общие media-типы без привязки к контейнеру и backend.

Отвечает за:

- `TrackId`;
- `TrackInfo`;
- `Packet`;
- `MediaTime`;
- `TimeBase`;
- `MediaInfo`;
- `ContainerKind`;
- `StreamDescriptor`;
- subtitle descriptors;
- audio/video track metadata;
- optional video coded/display width and height, если контейнер или manifest сообщает их до decode.

Текущие типы из `webm-demux::packet` должны переехать сюда.

### `codec-core`

Типизированная модель codec/profile/capability.

Отвечает за:

- `VideoCodec`;
- `AudioCodec`;
- `VideoProfile`;
- `CodecLevel`;
- `BitDepth`;
- `ChromaSubsampling`;
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
- normalization codec ids из контейнеров и сервисов.

Не отвечает за:

- прямое чтение VA-API;
- запуск decode backend;
- ad-hoc bitstream parsing в обход codec backend parser'ов.

Если capability selection требует profile/bit-depth/chroma/resolution из bitstream, `codec-core` должен хранить typed модель и conversion helpers, а сам parser должен жить рядом с codec/backend integration или быть адаптером над уже используемым parser'ом.

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
Он выбирает текущий поддержанный SDR VP9/Opus WebM selector, получает direct
media URLs/headers и отдаёт player shell-у готовый streaming demuxer. Полная
Rust-замена extractor-а остаётся будущей задачей этого же crate-а.

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

Сейчас функциональность живет в `webm-demux`. Позже crate можно переименовать или оставить, но media-типы должны уйти в `media-core`.

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

Со временем crate может быть разделен на `audio-core`, `audio-codecs`, `audio-output-cpal`, если станет слишком большим.

### `video-core`

Общие video decode/render bridge-типы.

Отвечает за:

- decoded frame descriptor;
- frame handles;
- color metadata;
- decoded pixel format;
- bit depth/chroma metadata;
- decoded memory path, например `DmaBufZeroCopy` или `CpuUpload`;
- video frame lifecycle contracts;
- backend-independent decode traits.

Не должен знать о VA-API, DX12, VideoToolbox или egui.

### `video-backend-vaapi`

Linux hardware decode backend.

Сейчас это `video-vaapi`. Целевое имя можно выбрать позже. Ответственность:

- VA-API display open;
- i965/iHD probing;
- codec/profile capability scan;
- adapter'ы над codec parser'ами, если decode path уже содержит проверенный parser;
- hardware decode;
- frame pool;
- DMA-BUF/texture upload integration;
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
- P010/HDR shader path в Phase 10;
- mapping typed color metadata to GPU uniforms;
- SDR color adjustments in shader uniforms;
- HDR-to-SDR tone mapping в Phase 10;
- distinction between P010 zero-copy boundary readiness and production P010 renderability;
- egui composition.

Текущее состояние: production NV12 path уже живёт в `render-wgpu`; P010 zero-copy boundary существует как diagnostic readiness, а production P010/HDR renderer остаётся задачей Phase 10.

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

### `storage`

SQLite persistent storage.

Отвечает за:

- `~/.local/share/rustiplayer/rustiplayer.sqlite`;
- migrations;
- history;
- bookmarks;
- playlists;
- media metadata cache;
- service account/session/cookies;
- playback progress;
- capability cache;
- crash/error reports.

Используем `rusqlite`.

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
  -> storage

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
- `storage -> app-egui`.
