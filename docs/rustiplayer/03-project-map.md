# 03. Карта проекта

## Workspace crates

```text
crates/animation-core        neutral UI animation math
crates/app-egui              desktop shell, media opening, backend/render wiring
crates/player-core           worker, session, scheduler, commands/events/snapshots
crates/audio-core            neutral audio decoder/output/clock contracts
crates/media-core            Packet, TrackInfo, MediaTime, TimelineSnapshot
crates/codec-core            codec/profile/color/stream requirement contracts
crates/video-frame-contract  neutral decoded frame layout/transfer contract vocabulary
crates/capability-core       backend reports, render reports, stream selection
crates/config                TOML schema v3, defaults, validation, paths
crates/source-core           local files, HTTP Range, RAM byte-range cache
crates/media-prefetch        config-agnostic RAM read-ahead over ByteSource
crates/settings-core         neutral settings metadata/controller contracts
crates/settings-derive       proc-macro generated settings registry/accessors
crates/rustiplayer-settings  AppConfig/settings runtime binding contracts
crates/symphonia-demux       Symphonia audio container demux adapter
crates/webm-demux            compatibility re-export for old demux crate path
crates/service-direct-media  direct HTTP media opener over source/prefetch/demux
crates/service-youtube       yt-dlp adapter, stream candidates, YouTube demux open
crates/audio                 Symphonia/Opus decoder factory, CPAL output backend, audio clock
crates/video-core            decoded frame and video diagnostics contracts
crates/video-backend-api     backend startup and resource-provider contracts
crates/video-vaapi           VA-API decoder thread, probe, DMA-BUF export/lifecycle
crates/video-ffmpeg          optional FFmpeg software decoder, probe, AVFrame HostPlanar owner
crates/render-core           renderer-neutral capabilities and color contracts
crates/render-wgpu-video     WGPU DMA-BUF + HostPlanar materialization, YUV shaders
crates/render-wgpu-shell     WGPU device/surface shell, egui composition, present path
crates/desktop-integration   PlayerCommand/PlayerSnapshot adapter for desktop controls
crates/vp9-parser            VP9 header parser used by codec adapter
```

Local replacement crates outside workspace:

```text
crates/cros-codecs-patch     local patched cros-codecs dependency
crates/cros-libva-patch      local patched cros-libva dependency
```

## Владение

`app-egui` owns UI state, window lifecycle, renderer lifetime and current
production composition. It opens local files through `symphonia-demux`, receives
YouTube demuxers from `service-youtube`, selects VA-API or FFmpeg software plans
from renderer-intersected capabilities, creates the matching WGPU materializer
and forwards prepared boundaries into `player-core`. It must not own playback
queues, A/V scheduling or `PlayerSession` state.

`player-core` owns playback state. It consumes `PreparedMedia`, demuxer traits,
audio-core decoder/output contracts, video backend handles, commands, events and
snapshots. It no longer opens WebM/Matroska or imports `video-vaapi` directly,
and it does not materialize WGPU texture views. Present-frame resource lookup and
release use renderer-neutral handles from `video-backend-api`.

`media-core`, `codec-core`, `audio-core`, `video-frame-contract`, `render-core`,
`video-core` and `video-backend-api` are contract crates. They should stay
backend-neutral and avoid UI/service dependencies.

`video-frame-contract` owns the decoded frame contract vocabulary shared by
decoder, capability and renderer layers: `VideoFramePixelLayout`,
`VideoFrameContract`, `VideoFrameTransferPath`, `HardwareFrameHandle` and
`DmaBufImageLayout`. It is intentionally lower than `video-core` and does not
depend on codec, video, render, backend, FFmpeg, WGPU, VA-API, cros-codecs,
`player-core` or app crates.

`video-backend-api` owns the video backend startup/resource-provider contract:
`VideoBackendFactory`, `StartedVideoBackend`, `PresentFrameResourceProvider` and
the cloneable provider handle used by render lease accounting. It depends on
`video-core`, but must not depend on `player-core`, VA-API, WGPU or renderer
materialization code.

`source-core` owns byte access. It does not know YouTube, containers, codecs,
renderer or player state.

`media-prefetch` owns config-agnostic RAM read-ahead over `source-core::ByteSource`.
It owns the sliding window state and background worker boundary, but it does not
know YouTube, config schema, containers, codecs, renderer, UI or player state.

`service-youtube` owns YouTube/yt-dlp details. It returns already opened streaming
media/demuxer objects to the shell and does not contain UI/render/player state.
It exposes capability-aware candidate metadata so `app-egui` can select through
`capability-core` before asking the service to open the chosen stream.

`video-vaapi` owns VA-API decode, DMA-BUF export and decoded surface lifecycle
until renderer release. Its `VaapiVideoBackendFactory` implements the `video-backend-api`
`VideoBackendFactory` boundary. The concrete backend crate depends on
`video-backend-api`, not on `player-core`, `wgpu`, `wgpu-types` or `ash`.

`video-ffmpeg` owns FFmpeg runtime probe, raw FFmpeg FFI, software-only decoder
thread, padded packet/frame ownership, codec/pixel/color adapters and
AVFrame-backed HostPlanar resource lifetime. It exposes only neutral backend and
capability contracts to neighboring crates. Default workspace builds do not
enable feature `ffmpeg`.

`render-wgpu-video` owns the pure WGPU video backend: renderer capabilities,
NV12/P010 renderers, BT.2446-C HDR-to-SDR shader path, renderer-side DMA-BUF
import/materialization API, renderer-side HostPlanar upload textures/cache and
renderer diagnostics. It consumes renderer-neutral metadata plus duplicated
resource descriptors and does not call demux/source/player APIs or depend on
`video-vaapi`/`video-ffmpeg`.

`render-wgpu-shell` owns WGPU instance/device/surface lifecycle, required WGPU
feature selection for the video renderer, egui composition, frame timing and
submit/present. It consumes `render-wgpu-video`, but does not own decoded video
resources or playback queues.

## Направление зависимостей

Фактические direct dependency boundaries после refactor PR:

```text
app-egui -> player-core/service-youtube/desktop-integration
app-egui -> symphonia-demux/service-direct-media/audio/video-core/video-frame-contract/video-vaapi/video-ffmpeg/render-wgpu-shell/render-wgpu-video/source-core
app-egui -> media-core/capability-core/render-core/rustiplayer-config/rustiplayer-settings/settings-core/animation-core
app-egui -> wgpu/winit/egui/egui-winit
player-core -> media-core/codec-core/capability-core/video-core/video-backend-api/video-frame-contract/rustiplayer-config/audio-core/render-core
desktop-integration -> player-core/media-core
service-youtube -> source-core/symphonia-demux/rustiplayer-config/capability-core/codec-core/media-core
service-youtube -> media-prefetch
service-direct-media -> source-core/media-prefetch/symphonia-demux/rustiplayer-config
source-core -> rustiplayer-config
media-prefetch -> source-core
rustiplayer-settings -> player-core/render-core/rustiplayer-config/settings-core
settings-derive -> settings-core/proc-macro2/quote/syn
symphonia-demux -> media-core/codec-core/source-core
webm-demux -> symphonia-demux
audio -> audio-core
capability-core -> codec-core/render-core/video-frame-contract
codec-core -> vp9-parser/video-frame-contract
video-frame-contract -> serde
video-core -> media-core/codec-core/video-frame-contract
video-backend-api -> video-core
render-core -> codec-core/video-frame-contract
video-vaapi -> video-backend-api/video-core/video-frame-contract/media-core/codec-core/capability-core
video-ffmpeg -> video-backend-api/video-core/video-frame-contract/codec-core/capability-core
render-wgpu-video -> render-core/video-core/video-backend-api/video-frame-contract/codec-core/wgpu/ash/wgpu-types
render-wgpu-shell -> render-wgpu-video/render-core/wgpu/egui/egui-wgpu/winit
```

Contract crates should not depend on `app-egui`, `service-youtube` or concrete UI.

## Before/after dependency map

Краткая карта изменений, которые уже произошли в refactor PR:

```text
Before:
  player-core -> webm-demux
  player-core -> video-vaapi
  player-core -> wgpu
  render-wgpu -> egui/winit/video-vulkan

After:
  app-egui -> symphonia-demux
  app-egui -> video-vaapi
  app-egui -> render-wgpu-shell/render-wgpu-video
  player-core -> video-backend-api
  decoder/render transfer vocabulary -> video-frame-contract
  capability selection -> SupportedVideoOutput + VideoFrameContract
  video-vaapi -> video-backend-api
  reverse video-vaapi/player-core edge closed
  player-core -> wgpu closed
  render-wgpu split into render-wgpu-video and render-wgpu-shell
  video-vulkan removed from workspace and Cargo graph
  video-ffmpeg added as isolated optional FFmpeg software backend
  render-wgpu-video added renderer-side HostPlanar upload/materialization
```

Закрытые связи `player-core -> symphonia-demux/webm-demux` и `player-core -> video-vaapi` не
должны возвращаться. Обратная direct dependency от `video-vaapi` к `player-core`
тоже не должна возвращаться: backend startup/resource-provider boundary живёт в
`video-backend-api`. Concrete factory и WGPU materialization собираются в
shell/render composition layer-е: VA-API использует DMA-BUF materializer, а
FFmpeg software использует HostPlanar upload materializer.

## Особые случаи

- `desktop-integration` depends on `player-core` and `media-core`, because it is
  an adapter from desktop protocols to public player contracts.
- `video-vaapi::VaapiVideoBackendFactory` is the production backend factory.
  It returns only a neutral `StartedVideoBackend`; WGPU materialization is owned
  by `render-wgpu-video`.
- `video-ffmpeg::FfmpegSoftwareVideoBackendFactory` is the optional software
  backend factory. It returns only a neutral `StartedVideoBackend`; raw FFmpeg
  handles stay inside `video-ffmpeg`.
- `video-backend-api` owns `VideoBackendFactory`, `StartedVideoBackend` and the
  renderer-neutral resource-provider handle used by `player-core`.
- WGPU texture-view materialization stays in `render-wgpu-video`, and WGPU
  surface presentation stays in `render-wgpu-shell`; `app-egui` only wires the
  concrete backend/materializer. `player-core` exposes opaque frame descriptors
  and resource lookup/release accounting only.
- The old reference backend `video-vulkan` has been removed from workspace and
  must not return as an implicit production dependency.
- Миграция Symphonia закрыла активный долг локального fork-а: workspace
  использует upstream `symphonia = 0.6` из Cargo, а устаревшие локальные каталоги
  патчей Symphonia удалены. Оставшиеся локальные dependency patches - это
  compatibility patches `cros-codecs`/`cros-libva`, перечисленные выше.
