# 03. Карта проекта

## Workspace crates

```text
crates/app-egui              desktop shell, media opening, backend/render wiring
crates/player-core           worker, session, scheduler, commands/events/snapshots
crates/media-core            Packet, TrackInfo, MediaTime, TimelineSnapshot
crates/codec-core            codec/profile/color/surface/memory contracts
crates/capability-core       backend reports, render reports, stream selection
crates/config                TOML schema v2, defaults, validation, paths
crates/source-core           local files, HTTP Range, RAM byte-range cache
crates/webm-demux            Symphonia WebM/Matroska demuxer
crates/service-youtube       yt-dlp adapter, stream candidates, YouTube demux open
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
third_party/symphonia-*      retired Symphonia patches, kept for cleanup PR
```

## Владение

`app-egui` owns UI state, window lifecycle, renderer lifetime and current
production composition. It opens local files through `webm-demux`, receives
YouTube demuxers from `service-youtube`, creates the VA-API/WGPU backend factory
and forwards prepared boundaries into `player-core`. It must not own playback
queues, A/V scheduling or `PlayerSession` state.

`player-core` owns playback state. It consumes `PreparedMedia`, demuxer traits,
audio output/decoder services, video backend handles, commands, events and
snapshots. It no longer opens WebM/Matroska or imports `video-vaapi` directly,
but it still contains WGPU-specific render texture boundary types for the
current zero-copy render path.

`media-core`, `codec-core`, `render-core`, `video-core` are contract crates.
They should stay backend-neutral and avoid UI/service dependencies.

`source-core` owns byte access. It does not know YouTube, containers, codecs,
renderer or player state.

`service-youtube` owns YouTube/yt-dlp details. It returns already opened streaming
media/demuxer objects to the shell and does not contain UI/render/player state.
It also exposes capability-aware candidate metadata for a future startup path
where `capability-core` selects before the demuxer is opened.

`video-vaapi` owns VA-API decode, DMA-BUF export/import and texture pool lifetime.
Its `VaapiWgpuVideoBackendFactory` implements the `player-core`
`VideoBackendFactory` boundary, so the concrete backend crate currently depends
on `player-core` for the adapter contract.

`render-wgpu` owns WGPU resources and shader paths. It consumes renderer-neutral
metadata plus WGPU texture views and does not call demux/source/player APIs. The
crate still also contains egui/winit shell composition and a `video-vulkan`
reference dependency.

## Направление зависимостей

Фактические direct dependency boundaries после refactor PR:

```text
app-egui -> player-core/service-youtube/desktop-integration
app-egui -> webm-demux/audio/video-vaapi/render-wgpu/source-core
app-egui -> media-core/codec-core/capability-core/video-core/render-core/rustiplayer-config
app-egui -> wgpu/winit/egui/egui-winit
player-core -> media-core/codec-core/capability-core/video-core/rustiplayer-config/audio/wgpu
desktop-integration -> player-core/media-core
service-youtube -> source-core/webm-demux/rustiplayer-config/capability-core/codec-core
source-core -> rustiplayer-config
webm-demux -> media-core/codec-core/source-core
audio -> codec-core
capability-core -> codec-core/render-core
video-core -> media-core/codec-core
render-core -> codec-core
video-vaapi -> player-core/video-core/media-core/codec-core/capability-core/wgpu
render-wgpu -> render-core/video-core/codec-core/video-vulkan/wgpu/egui/egui-wgpu/winit
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
  app-egui -> webm-demux
  app-egui -> video-vaapi
  video-vaapi -> player-core (adapter for VideoBackendFactory)
  player-core -> wgpu remains temporary
  render-wgpu -> egui/egui-wgpu/winit/video-vulkan remains temporary
```

Закрытые связи `player-core -> webm-demux` и `player-core -> video-vaapi` не
должны возвращаться. Текущий production path всё ещё WGPU/VA-API specific, потому
что zero-copy texture lookup и concrete backend factory завязаны на эти API.

## Особые случаи

- `desktop-integration` depends on `player-core` and `media-core`, because it is
  an adapter from desktop protocols to public player contracts.
- `video-vaapi::VaapiWgpuVideoBackendFactory` is the production backend factory.
  The deprecated `WgpuVideoBackendFactory` alias now lives in `video-vaapi`, not
  in `player-core`.
- `player-core` still exposes `WgpuRenderTextureProviderHandle` and related
  WGPU texture-view lookup types. This is an unresolved renderer-neutrality
  boundary.
- `video-vulkan` remains in the workspace but is not the production decode path
  used by `PlayerWorker`; `render-wgpu` still depends on it as reference debt.
