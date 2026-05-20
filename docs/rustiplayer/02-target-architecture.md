# 02. Целевая архитектура

## Слои

```text
app-egui
  window, egui, input, redraw pacing, local/YouTube media opening,
  WGPU surface, VA-API/WGPU backend wiring
        |
        +--------------------+--------------------+--------------------+
        |                    |                    |                    |
        v                    v                    v                    v
player-core          service-youtube      video-vaapi          render-wgpu
  worker/session      yt-dlp adapter,     VA-API decode,       WGPU shell,
  commands/events     HTTP refresh,       DMA-BUF export,      egui/winit
  scheduler/state     WebM demux open     WGPU import          composition
        |                    |                    |                    |
        v                    v                    v                    v
media-core           source-core          video-core           render-core
  packets/tracks      local/http/cache    decoded frame        render/color
        |                    |            contract             contract
        v                    v                    |                    |
codec-core <--------- webm-demux <--------+       |                    |
  codec/color         packets/tracks              |                    |
        |                                           v                    v
        +------------------------------------> capability-core <---------+
                                              decode/render selection
```

## Runtime flow

```text
UI/CLI/media URL
  -> app-egui local opener или service-youtube startup job
  -> PreparedMedia или already-opened streaming demuxer
  -> PlayerCommand / PlayerWorker command
  -> PlayerWorker bounded channels
  -> PlayerSession state machine
  -> demux/audio/video scheduler
  -> PlayerSnapshot + PlayerWorkerEvent
  -> egui, renderer, desktop integration
```

`PlayerWorker` владеет `PlayerSession` на отдельном thread. `app-egui` отправляет
команды, читает latest snapshot/events и не вызывает `PlayerSession::tick()`
напрямую.

Текущий production composition не является полностью backend-neutral:
`app-egui` создаёт WGPU context, регистрирует VA-API capability probe, открывает
локальный WebM через `webm-demux` и передаёт в `player-core` уже подготовленный
`PreparedMedia`. YouTube startup остаётся service-level открытием demuxer-а через
`service-youtube`.

## Video flow

```text
Demuxer::next_packet()
  -> media_core::Packet { Bytes payload, TrackId, PTS, keyframe }
  -> codec-core requirement/refinement
  -> capability-core intersection
  -> app-egui VaapiWgpuVideoBackendFactory
  -> player-core VideoBackendFactory boundary
  -> video-vaapi::VideoDecodeThread
  -> video_core::DecodedFrame
  -> PlayerWorker PresentFrameLease
  -> render-wgpu::WgpuRenderableFrame
  -> WGPU swapchain
```

Production frame memory path: `FrameMemoryPath::DmaBufZeroCopy`. Production
decode/render path: VA-API decode, DMA-BUF export, WGPU texture import,
`render-wgpu` NV12/P010 rendering.

## Color flow

```text
manifest/container/bitstream/backend metadata
  -> codec_core::VideoColorMetadata
  -> video_core::DecodedFrame
  -> render_core::RenderableFrame
  -> render-wgpu NV12 or P010 renderer
  -> RenderDiagnostics::active_color_path
```

SDR/NV12 и HDR/P010 не смешаны в одном shader-е. NV12 использует
`nv12_to_rgba.wgsl`; P010/HDR использует `p010_bt2446c_to_sdr.wgsl`.

## Capability flow

Capability selection проходит четыре проверки:

1. stream requirement из manifest/container/codec adapter;
2. hardware decode format из backend probe;
3. mandatory memory contract `DMA-BUF`;
4. renderer support для decoded format, P010 layout и color path.

Typed reject лучше позднего decoder/render crash. Recoverable или неполный probe
не должен блокировать потенциально воспроизводимый stream.

Capability report собирается shell/backend слоем: `app-egui` запускает
`CapabilityScanner`, регистрирует VA-API provider и render capabilities, затем
передаёт `SystemCapabilities` в worker. `service-youtube` уже умеет строить
capability-aware stream candidates из manifest metadata, но текущий startup path
ещё открывает demuxer напрямую через SDR-safe selector. Поздний выбор YouTube
candidate-а через `capability-core` остаётся extension point.

## Render lease flow

```text
PlayerWorker::try_acquire_present_frame()
  -> PresentFrameLease { DecodedFrame, generation, stale, texture handle }
  -> PresentFrameLease::texture_views()
  -> WgpuRenderableFrame::from_decoded_nv12/from_decoded_p010
  -> Renderer::render_frame()
  -> RAII drop/ack releases texture slot
```

Renderer errors возвращаются в worker как `PlayerRenderError`. Silent black-frame
fallback допустим только когда кадра ещё нет; нарушение render contract должно
становиться typed error.
