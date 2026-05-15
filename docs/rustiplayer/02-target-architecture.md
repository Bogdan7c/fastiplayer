# 02. Целевая архитектура

## Слои

```text
app-egui
  window, egui, input, redraw pacing, renderer wiring
        |
        v
player-core
  PlayerWorker, PlayerSession, commands, events, snapshots, scheduler
        |
        +------------------+------------------+------------------+
        |                  |                  |                  |
        v                  v                  v                  v
source-core          webm-demux          audio              video-vaapi
local/http/cache     packets/tracks      Opus/CPAL         VA-API decode thread
        |                  |                  |                  |
        v                  v                  v                  v
service-youtube      media-core          codec-core         video-core
yt-dlp adapter       neutral media       codec/color        decoded frame contract
                                           |
                                           v
capability-core ----------------------> render-core
decode/render intersection              renderer-neutral contracts
                                           |
                                           v
render-wgpu
WGPU shell, NV12 renderer, P010 HDR-to-SDR renderer
```

## Runtime flow

```text
UI/CLI/media URL
  -> PlayerCommand или shell startup job
  -> PlayerWorker bounded channels
  -> PlayerSession state machine
  -> source/demux/audio/video scheduler
  -> PlayerSnapshot + PlayerWorkerEvent
  -> egui, renderer, desktop integration
```

`PlayerWorker` владеет `PlayerSession` на отдельном thread. `app-egui` отправляет
команды, читает latest snapshot/events и не вызывает `PlayerSession::tick()`
напрямую.

## Video flow

```text
Demuxer::next_packet()
  -> media_core::Packet { Bytes payload, TrackId, PTS, keyframe }
  -> codec-core requirement/refinement
  -> capability-core intersection
  -> video-vaapi::VideoDecodeThread
  -> video_core::DecodedFrame
  -> PlayerWorker PresentFrameLease
  -> render-wgpu::WgpuRenderableFrame
  -> WGPU swapchain
```

Production frame memory path: `FrameMemoryPath::DmaBufZeroCopy`.

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
