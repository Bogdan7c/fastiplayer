# 02. Целевая архитектура

## Слои

```text
app-egui
  window, egui, input, redraw pacing, local/YouTube media opening,
  WGPU surface, VA-API/FFmpeg backend and WGPU materializer wiring
        |
        +--------------------+--------------------+--------------------+--------------------+
        |                    |                    |                    |                    |
        v                    v                    v                    v                    v
player-core          service-youtube      video-vaapi          render-wgpu-shell   render-wgpu-video
  worker/session      yt-dlp adapter,     VA-API decode,       WGPU surface,       NV12/P010 renderer,
  commands/events     HTTP refresh,       DMA-BUF export,      egui/winit          materialization API,
  scheduler/state     stream demux open   resource lifetime    composition         shader/color path
        |                    |                    |                    |                    |
        v                    v                    v                    v                    v
media-core           source-core          video-backend-api    render-core <-------+
  packets/tracks      local/http/cache    startup/backend     render/color
        |                    |            startup/resource     contract
        v                    v            provider contract           |
codec-core <--------- symphonia-demux <---+       |                    |
  codec/color         packets/tracks              |                    |
        |                                           v                    v
        |                                     video-core                |
        |                                    decoded frame              |
        |                                      contract                 |
        |                                           |                   |
        |                                           v                   v
        +------------------------------------> capability-core <---------+
                                              decode/render selection

video-ffmpeg is an optional sibling concrete backend behind the same
`video-backend-api` boundary: FFmpeg software decode -> HostPlanar frame
descriptor -> render-wgpu-video HostPlanar upload.
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
`app-egui` создаёт WGPU context, регистрирует VA-API и FFmpeg capability
providers, открывает локальные media files через `symphonia-demux` и передаёт в
`player-core` уже подготовленный `PreparedMedia`. YouTube startup остаётся
service-level открытием demuxer-а через `service-youtube`.

## Video flow

```text
Demuxer::next_packet()
  -> media_core::Packet { Bytes payload, TrackId, PTS, keyframe }
  -> codec-core requirement/refinement
  -> capability-core intersection
  -> app-egui VaapiVideoBackendFactory или FfmpegSoftwareVideoBackendFactory
  -> video-backend-api VideoBackendFactory / StartedVideoBackend
  -> player-core playback-facing decoder handle
  -> video-vaapi::VideoDecodeThread или video-ffmpeg decoder thread
  -> video_core::DecodedFrame
  -> PlayerWorker PresentFrameLease + resource descriptor
  -> app-egui/render-wgpu-video WGPU materialization
  -> render_wgpu_video::WgpuRenderableFrame
  -> render-wgpu-shell Renderer
  -> WGPU swapchain/surface present
```

Hardware frame memory path: `FrameMemoryPath::DmaBufZeroCopy`. Software frame
memory path: `FrameMemoryPath::HostUpload`. Hardware decode/render path:
VA-API decode, DMA-BUF export, WGPU texture import, `render-wgpu-video`
NV12/P010 rendering and `render-wgpu-shell` surface present. Software
decode/render path: FFmpeg software decode, AVFrame-backed HostPlanar resource,
one host-to-GPU upload, GPU YUV sampling/color/HDR path and the same shell
present path.

## Color flow

```text
manifest/container/bitstream/backend metadata
  -> codec_core::VideoColorMetadata
  -> video_core::DecodedFrame
  -> render_core::RenderableFrame
  -> render-wgpu-video NV12/P010 or HostPlanar YUV renderer
  -> RenderDiagnostics::active_color_path
```

SDR/NV12 и HDR/P010 не смешаны в одном shader-е. NV12 использует
`nv12_to_rgba.wgsl`; P010/HDR использует `p010_bt2446c_to_sdr.wgsl`.

## Capability flow

Capability selection проходит четыре проверки:

1. stream requirement из manifest/container/codec adapter;
2. decode format из backend probe;
3. mandatory frame contract: DMA-BUF zero-copy for hardware, SoftwareHostUpload
   for FFmpeg software;
4. renderer support для decoded format, transfer path, P010/HostPlanar layout и
   color path.

Typed reject лучше позднего decoder/render crash. Recoverable или неполный probe
не должен блокировать потенциально воспроизводимый stream.

Capability report собирается shell/backend слоем: `app-egui` запускает
`CapabilityScanner`, регистрирует VA-API provider, FFmpeg software provider и
render capabilities, затем передаёт `SystemCapabilities` в worker.
Для YouTube startup `app-egui` получает capability-aware stream candidates из
`service-youtube`, выбирает поток через `capability-core`, затем просит service
открыть только выбранный stream id.

## Render lease flow

```text
PlayerWorker::try_acquire_present_frame()
  -> PresentFrameLease { DecodedFrame, generation, stale, texture handle }
  -> PresentFrameLease::resource_descriptor() / try_resource_lookup()
  -> app-egui WgpuFrameTextureViewMaterializer
  -> render-wgpu-video WgpuFrameTextureViews
  -> WgpuRenderableFrame::from_decoded_nv12/from_decoded_p010/from_decoded_host_yuv
  -> render-wgpu-shell::Renderer::render_frame()
  -> RAII drop/ack releases texture slot
```

Renderer errors возвращаются в worker как `PlayerRenderError`. Silent black-frame
fallback допустим только когда кадра ещё нет; нарушение render contract должно
становиться typed error.
