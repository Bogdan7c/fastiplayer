# 03. Карта проекта

## Workspace crates

```text
crates/app-egui              desktop shell, egui, winit, render wiring
crates/player-core           worker, session, scheduler, commands/events/snapshots
crates/media-core            Packet, TrackInfo, MediaTime, TimelineSnapshot
crates/codec-core            codec/profile/color/surface/memory contracts
crates/capability-core       backend reports, render reports, stream selection
crates/config                TOML schema v2, defaults, validation, paths
crates/source-core           local files, HTTP Range, RAM byte-range cache
crates/webm-demux            Symphonia WebM/Matroska demuxer
crates/service-youtube       temporary yt-dlp service adapter
crates/audio                 Opus decoder, CPAL output, audio clock
crates/video-core            decoded frame and video diagnostics contracts
crates/video-vaapi           VA-API decoder thread, probe, DMA-BUF/WGPU import
crates/render-core           renderer-neutral capabilities and color contracts
crates/render-wgpu           WGPU shell, NV12 path, P010 BT.2446-C path
crates/desktop-integration   PlayerCommand/PlayerSnapshot adapter for desktop controls
crates/vp9-parser            VP9 header parser used by codec adapter
crates/video-vulkan          reference/experimental Vulkan Video code
crates/cros-codecs-patch     local patched cros-codecs dependency
crates/cros-libva-patch      local patched cros-libva dependency
third_party/symphonia-*      local patched Symphonia crates
```

## Владение

`app-egui` owns UI state and renderer lifetime. It must not own demuxer, decoder
state, playback queues or A/V scheduling.

`player-core` owns playback state. It may orchestrate source/demux/audio/video,
but backend-specific initialization should stay behind factory/boundary types.

`media-core`, `codec-core`, `render-core`, `video-core` are contract crates.
They should stay backend-neutral and avoid UI/service dependencies.

`source-core` owns byte access. It does not know YouTube, containers, codecs,
renderer or player state.

`service-youtube` owns YouTube/yt-dlp details. It returns already opened streaming
media/demuxer objects to the shell and does not contain UI/render/player state.

`video-vaapi` owns VA-API decode, DMA-BUF export/import and texture pool lifetime.

`render-wgpu` owns WGPU resources and shader paths. It consumes renderer-neutral
metadata plus WGPU texture views and does not call demux/source/player APIs.

## Направление зависимостей

Allowed direction is mostly top-down:

```text
app-egui -> player-core -> media/codec/capability/audio/video-vaapi/webm-demux
app-egui -> render-wgpu -> render-core/video-core
service-youtube -> source-core/webm-demux/config
webm-demux -> media-core/codec-core/source-core
capability-core -> codec-core/render-core
video-vaapi -> video-core/codec-core/capability-core/wgpu
```

Contract crates should not depend on `app-egui`, `service-youtube` or concrete UI.

## Особые случаи

- `desktop-integration` depends on `player-core` and `media-core`, because it is
  an adapter from desktop protocols to public player contracts.
- `player-core::WgpuVideoBackendFactory` currently names WGPU handles but starts
  the VA-API decode backend. This is an unresolved naming/ownership boundary.
- `video-vulkan` remains in the workspace but is not the production decode path
  used by `PlayerWorker`.
