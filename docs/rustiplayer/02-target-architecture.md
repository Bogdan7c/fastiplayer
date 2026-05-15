# 02. Target Architecture

## Главный принцип

`app-egui` не должен быть плеером. Он должен быть оболочкой: окно, ввод, UI, отправка команд, получение snapshot'ов и вызов renderer.

Полноценная логика воспроизведения должна жить в `player-core` и соседних core/backend crate'ах.

## Целевая схема слоев

```text
app-egui
  window lifecycle, egui UI, command dispatch, render integration
        |
        v
player-core
  PlayerWorker boundary, state machine, commands, events, snapshots, playback tick, seek, EOF, errors
        |
        +----------------+----------------+----------------+
        |                |                |                |
        v                v                v                v
source-core       demux-*           audio-core       video pipeline
local/http/cache  webm/mp4/hls      software audio   hardware-only video
services          dash/ts/mov       decode/output    backend selection
        |                |                |                |
        v                v                v                v
service-youtube   media-core        audio backend    video-backend-vaapi
future services   tracks/packets    cpal/ringbuf     future dx12/vt
        |
        v
config/capabilities/runtime diagnostics
TOML, system probing, in-memory metrics
        |
        v
render-core -> render-wgpu
future render-gles
```

## Runtime data flow

```text
User input / CLI / media URL
        |
        v
PlayerCommand
        |
        v
PlayerWorker
  crossbeam-channel command queue, latest snapshot publisher, event stream,
  render frame lease requests, typed render errors, shutdown/join path
        |
        v
PlayerSession
        |
        +--> SourceResolver
        |       local file, HTTP range, YouTube service, future services
        |
        +--> DemuxSession
        |       container packets, track metadata, timestamps
        |
        +--> AudioPipeline
        |       software decode -> buffer -> audio clock
        |
        +--> VideoPipeline
        |       hardware decode only -> decoded frame handles
        |
        +--> AvScheduler
        |       audio-master sync, frame queue, drop/wait/present
        |
        v
PlayerSnapshot
        |
        +--> egui UI
        +--> renderer present frame lease
        +--> MPRIS state
        +--> telemetry
```

## Renderer color data flow

Цветовой путь должен быть отдельной частью renderer boundary, а не знанием внутри одного shader-а.

```text
container/service/bitstream hints
        |
        v
codec-core VideoColorMetadata
        |
        v
video-core DecodedFrame
  pixel format, bit depth, chroma, color metadata, zero-copy memory path, texture handle
        |
        v
render-core RenderableFrame
  renderer-neutral metadata, ColorPipelineSettings, ActiveColorPath
        |
        v
render-wgpu
  metadata/settings -> ColorPipelineUniforms -> NV12 shader path
  metadata/HdrToSdrSettings -> HdrColorPipelineUniforms -> P010/HDR BT.2446-C shader path
        |
        v
swapchain output
```

Текущий статус после Phase 10: Phase 8.5 сохранил SDR VP9/NV12 путь и сделал явными metadata/defaults/uniforms; Phase 9 закрыл VP9/P010 readiness; Phase 10 добавил отдельный P010/HDR BT.2446-C renderer без повторного рефакторинга `DecodedFrame` и `RenderableFrame`. Native HDR output не входит в текущий renderer path и остаётся future work.

Color metadata выбирается layered-моделью: manifest/container metadata используется как ранний hint, codec bitstream parser уточняет colorimetry, decoder/backend подтверждает фактический decoded format, а fallback явно помечается как fallback. Текущий fallback для старого SDR пути - `BT.709 limited SDR`.

## Bitstream metadata probing

Capability-based selection может уточнять stream requirement из container metadata, service manifest и codec bitstream headers.

Правило разделения ответственности:

- demux/source layer отдаёт metadata, которую контейнер или manifest сообщает без decode;
- codec adapter layer разбирает codec-specific headers: VP9 uncompressed header, AV1 sequence header OBU, H.264 SPS, H.265 VPS/SPS;
- `player-core` только оркестрирует generic probing API и принимает typed `VideoDecodeRequirement`;
- UI показывает итоговую причину отказа, но не содержит codec-specific logic.

Парсинг bitstream headers не должен жить в `app-egui`, demuxer-е или scheduler коду. Если для codec'а уже есть parser в используемом decode backend, нужно адаптировать его через `codec-core` adapter API, а не писать второй неполный bit-level parser.

Fatal capability reject допустим только после успешного и валидного разбора header'а. Если header неполный, packet не keyframe, parser вернул recoverable parse error или metadata нельзя проверить, probing должен быть мягким: логировать диагностическое событие и дать decoder-у продолжить работу. Нельзя превращать неуверенный parser output в user-facing `HardwareDecoderUnavailable`.

## Command/event model

`player-core` должен быть command-driven.

Примеры команд:

```rust
enum PlayerCommand {
    OpenMedia(MediaOpenRequest),
    Play,
    Pause,
    TogglePlayback,
    Seek(SeekRequest),
    BeginScrub,
    UpdateScrub(SeekRequest),
    PreviewScrub(SeekRequest),
    EndScrub { policy: ScrubCommitPolicy },
    Stop,
    SetVolume(f32),
    SelectVideoTrack(TrackId),
    SelectAudioTrack(TrackId),
    SelectSubtitleTrack(Option<TrackId>),
    SelectQuality(QualitySelection),
    ReloadConfig,
    Shutdown,
}
```

В schema v2 единственная commit policy - `CommitLatest`.

Worker-level `SeekController` хранит generation id, current mode, latest scrub
target, in-flight target, resume intent и diagnostics counters для stale/ignored
и cancelled операций. Реальный demux seek transaction уже живёт в
`PlayerSession`: video seek запрашивает decode-safe point before target, затем
player-core делает decoder reset, pre-roll/drop и commit gates до пользовательской
позиции. Command priority действует на worker boundary: Stop/Open/Shutdown
прерывают scrub, внешний `Seek` во время scrub игнорируется, Play/Pause меняют
resume intent, Volume/Mute применяются сразу.

Seek contract использует typed timeline values:

```rust
enum SeekTarget {
    Absolute(MediaTime),
    Relative(Duration),
}

struct SeekRequest {
    target: SeekTarget,
    mode: SeekMode,
}
```

`MediaTime`, `MediaDuration`, `TrackTimestamp`, `TimelineRange` и
`TimelineSnapshot` живут в `media-core`, потому что это нейтральная media model.
Они не знают о WebM, VP9, VA-API, wgpu, MPRIS, YouTube или конкретном network
source. Первые production adapters будут конкретными: WebM/Matroska и YouTube
VOD WebM для source/demux, VP9/Opus для codec/audio, VA-API для video backend,
wgpu для render и Linux MPRIS для desktop controls.

Примеры событий:

```rust
enum PlayerEvent {
    MediaOpened(MediaInfo),
    PlaybackStateChanged(PlaybackState),
    PositionChanged(MediaTime),
    VideoFrameReady(FramePresentationInfo),
    BufferingStateChanged(BufferingState),
    CapabilityScanCompleted(SystemCapabilities),
    RecoverableError(PlayerError),
    FatalError(PlayerError),
}
```

UI не должен напрямую дергать demuxer, audio output или VA-API. UI отправляет команды и рисует `PlayerSnapshot`.
В текущем runtime UI отправляет команды в `PlayerWorker`, а worker уже вызывает
`PlayerSession::dispatch_command`/`tick` и публикует latest snapshot/events.

## Render frame lease boundary

Render thread получает кадр через lease, а не через прямой доступ к
`PlayerSession` или `PlaybackPipeline`.

Текущий контракт:

- `PlayerWorker::try_acquire_present_frame()` возвращает `PresentFrameLease`
  через compatibility alias `PlayerPresentFrame`;
- lease содержит decoded frame metadata, opaque texture handle, render generation
  и stale flag;
- worker выбирает frame handle, но не создаёт `wgpu::TextureView`;
- texture views создаются на render thread через render-side provider:
  `PresentFrameLease::texture_views()`;
- создание `TextureView` не является texture copy; CPU/GPU copy fallback ради
  render bridge не добавляется;
- render thread измеряет latency участка `queue.submit()`/`present()` и передает
  sample в `PlayerWorker` через bounded non-blocking diagnostics channel;
- release идёт через shared RAII drop/ack, когда освобождён последний clone lease-а;
- если ack уже невозможно доставить из-за shutdown, lease fail-closed освобождает
  texture через provider исходного кадра;
- stale frame state приходит через snapshot/lease metadata и не инвалидирует
  inflight lease.

Render bridge errors не должны превращаться в silent black frame. Missing texture
views, rejected `WgpuRenderableFrame` и render device/surface failures отправляются
в worker как typed `PlayerRenderError`, публикуются как `PlayerWorkerEvent::RenderError`
и обновляют `PlayerSnapshot.last_error`.

## Snapshot model

`PlayerSnapshot` - read-only состояние для UI, renderer и desktop integration.

Snapshot должен содержать:

- playback state;
- media title/source;
- legacy duration/current position для совместимости текущего UI;
- typed `TimelineSnapshot`: current position, target position during seek/scrub,
  duration, seekable flag/range, not-seekable reason, seeking/scrubbing flags,
  stale frame flag;
- selected tracks;
- available qualities;
- active backend;
- current video frame handle;
- active color path;
- audio buffer status;
- video queue status;
- dropped/repeated/presented frames;
- last errors;
- capability summary.

Snapshot не должен содержать mutable handles к decoder/demuxer/audio output.

## Threading model

Текущая базовая модель после smooth playback Session 9:

- main thread: winit/egui/render command submission;
- player worker thread: владеет `PlayerSession`, demux/audio/video pipeline,
  выполняет media-clock-driven playback tick и публикует latest snapshot;
- video decode thread: blocking hardware decode + DMA-BUF zero-copy export/import;
- HTTP fetch threads/tasks: source/network layer;
- audio callback thread: CPAL-owned;
- long-running persistence is not part of the current runtime: durable seek/cache
  metadata and runtime index jobs are absent and must not block playback.

Worker wakeup не использует fixed 60Hz как источник cadence. Следующий wakeup
выбирается из текущего состояния pipeline:

- command/render/scrub/shutdown event;
- deadline первого queued video frame относительно audio/media clock;
- decoder readiness poll, пока decode thread отдаёт frames через неблокирующий
  receive path;
- immediate bounded catch-up, когда очереди ниже target и есть полезная
  demux/decode работа;
- редкий coarse progress fallback без video cadence semantics.

Важное правило: render loop не должен содержать бизнес-логику playback и не должен
вызывать `PlayerSession::tick()` напрямую. Он отправляет `PlayerCommand`,
забирает `PlayerSnapshot`/`PlayerWorkerEvent` из worker boundary и запрашивает
present frame через render lease. `wgpu::TextureView` создаются на render thread,
а не в worker.

## Runtime diagnostics boundary

Diagnostics живут в `player-core` и backend crates, а UI только читает готовый
snapshot или summary. Smooth playback incident должен разбираться по стадиям:

- source/demux read;
- decoder submit;
- hardware sync;
- DMA-BUF export/import и zero-copy surface pool;
- worker scheduler;
- render acquire;
- GPU submit/present;
- release acknowledgement/backpressure.

Debug summary должен показывать typed drop counters, typed pause counters,
`FrameMemoryPath`, queue depths, worker wakeup reason, `front_frame` diff,
surface pool pressure и worst latency по каждой стадии. Эти метрики не требуют
CPU readback и не меняют ownership decoded frame-ов.

## Backpressure model

Backpressure должен быть явной частью `player-core`, а не набором констант в app layer.

Контролируемые очереди:

- demux packet read budget;
- pending audio packet queue;
- pending video packet queue;
- decoder input budget;
- decoded frame queue;
- texture pool capacity;
- network read-ahead;
- cache write backlog.

Каждый лимит должен приходить из `PlayerConfig`, а не быть hardcoded в render loop.

## Error model

Ошибки делятся на:

- user-facing: можно показать в UI;
- recoverable: можно продолжать playback;
- fatal media error: текущий media не воспроизводится;
- fatal runtime error: приложение не может продолжать работу;
- diagnostic: пишется в telemetry/log.

Пример:

```rust
enum PlayerErrorKind {
    UnsupportedVideoCodec,
    UnsupportedVideoProfile,
    UnsupportedHdrMode,
    HardwareDecoderUnavailable,
    DemuxError,
    NetworkError,
    AudioDeviceUnavailable,
    UnsupportedRenderFormat,
    RenderDeviceLost,
    ConfigError,
}
```

Render bridge использует отдельную typed оболочку `PlayerRenderError` с kind вроде
`MissingTextureViews`, `UnsupportedFrameFormat` и `RenderDeviceLost`, а затем
мапит её в существующий `PlayerError` snapshot contract.

## Почему так

Такое разделение дает:

- расширение codec/backend без переписывания UI;
- тестирование scheduler/state machine без GPU и окна;
- Linux-first реализацию без блокировки Windows/macOS;
- возможность добавить YouTube/account/cache без загрязнения player loop;
- понятный путь от MVP к полноценному плееру.
