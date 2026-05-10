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
  state machine, commands, events, snapshots, playback tick, seek, EOF, errors
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
storage/config/capabilities
SQLite, TOML, system probing
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
        +--> renderer present frame
        +--> MPRIS state
        +--> telemetry/storage
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
  pixel format, bit depth, chroma, color metadata, texture handle
        |
        v
render-core RenderableFrame
  renderer-neutral metadata, ColorPipelineSettings, ActiveColorPath
        |
        v
render-wgpu
  metadata/settings -> ColorPipelineUniforms -> NV12/P010 shader path
        |
        v
swapchain output
```

Phase 8.5 сохраняет текущий SDR VP9/NV12 путь и не добавляет HDR support. Цель этапа - сделать явными metadata, defaults и shader uniforms, чтобы Phase 9 мог закрыть VP9/P010 readiness, а Phase 10 мог добавить P010/HDR renderer без повторного рефакторинга `DecodedFrame` и `RenderableFrame`.

Color metadata выбирается layered-моделью: manifest/container metadata используется как ранний hint, codec bitstream parser уточняет colorimetry, decoder/backend подтверждает фактический decoded format, а fallback явно помечается как fallback. Текущий fallback для старого SDR пути - `BT.709 limited SDR`.

## Bitstream metadata probing

Capability-based selection может уточнять stream requirement из container metadata, service manifest и codec bitstream headers.

Правило разделения ответственности:

- demux/source layer отдаёт metadata, которую контейнер или manifest сообщает без decode;
- codec/backend layer разбирает codec-specific headers: VP9 uncompressed header, AV1 sequence header OBU, H.264 SPS, H.265 VPS/SPS;
- `player-core` только оркестрирует probing и принимает typed `VideoDecodeRequirement`;
- UI показывает итоговую причину отказа, но не содержит codec-specific logic.

Парсинг bitstream headers не должен жить в `app-egui` или расползаться по scheduler коду. Если для codec'а уже есть parser в используемом decode backend, нужно адаптировать его, а не писать второй неполный bit-level parser.

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
    SetVolume(f32),
    SelectVideoTrack(TrackId),
    SelectAudioTrack(TrackId),
    SelectSubtitleTrack(Option<TrackId>),
    SelectQuality(QualitySelection),
    ReloadConfig,
    Shutdown,
}
```

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

## Snapshot model

`PlayerSnapshot` - read-only состояние для UI, renderer и desktop integration.

Snapshot должен содержать:

- playback state;
- media title/source;
- duration/current position;
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

Базовая модель:

- main thread: winit/egui/render command submission;
- player tick: пока может жить на main thread, но как отдельный объект;
- video decode thread: blocking hardware decode/upload;
- HTTP fetch threads/tasks: source/network layer;
- audio callback thread: CPAL-owned;
- storage thread optional: SQLite writes can be batched later.

Важное правило: render loop не должен содержать бизнес-логику playback. Он вызывает `player.tick()` и передает результат в renderer.

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
    RenderDeviceLost,
    StorageError,
    ConfigError,
}
```

## Почему так

Такое разделение дает:

- расширение codec/backend без переписывания UI;
- тестирование scheduler/state machine без GPU и окна;
- Linux-first реализацию без блокировки Windows/macOS;
- возможность добавить YouTube/account/cache без загрязнения player loop;
- понятный путь от MVP к полноценному плееру.
