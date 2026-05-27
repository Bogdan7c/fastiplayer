# 03. Карта проекта

## Workspace crates

```text
crates/app-egui              desktop shell, media opening, backend/render wiring
crates/player-core           worker, session, scheduler, commands/events/snapshots
crates/audio-core            neutral audio decoder/output/clock contracts
crates/media-core            Packet, TrackInfo, MediaTime, TimelineSnapshot
crates/codec-core            codec/profile/color/surface/memory contracts
crates/capability-core       backend reports, render reports, stream selection
crates/config                TOML schema v2, defaults, validation, paths
crates/source-core           local files, HTTP Range, RAM byte-range cache
crates/symphonia-demux       Symphonia audio container demux adapter
crates/webm-demux            compatibility re-export for old demux crate path
crates/service-youtube       yt-dlp adapter, stream candidates, YouTube demux open
crates/audio                 Symphonia/Opus decoder factory, CPAL output backend, audio clock
crates/video-core            decoded frame and video diagnostics contracts
crates/video-backend-api     backend startup and resource-provider contracts
crates/video-vaapi           VA-API decoder thread, probe, DMA-BUF/WGPU import
crates/render-core           renderer-neutral capabilities and color contracts
crates/render-wgpu           WGPU shell, NV12 path, P010 BT.2446-C path
crates/desktop-integration   PlayerCommand/PlayerSnapshot adapter for desktop controls
crates/vp9-parser            VP9 header parser used by codec adapter
crates/video-vulkan          reference/experimental Vulkan Video code
crates/cros-codecs-patch     local patched cros-codecs dependency
crates/cros-libva-patch      local patched cros-libva dependency
```

## Владение

`app-egui` owns UI state, window lifecycle, renderer lifetime and current
production composition. It opens local files through `symphonia-demux`, receives
YouTube demuxers from `service-youtube`, creates the VA-API/WGPU backend factory
and forwards prepared boundaries into `player-core`. It must not own playback
queues, A/V scheduling or `PlayerSession` state.

`player-core` owns playback state. It consumes `PreparedMedia`, demuxer traits,
audio-core decoder/output contracts, video backend handles, commands, events and
snapshots. It no longer opens WebM/Matroska or imports `video-vaapi` directly,
and it does not materialize WGPU texture views. Present-frame resource lookup and
release use renderer-neutral handles from `video-backend-api`.

`media-core`, `codec-core`, `audio-core`, `render-core`, `video-core` and
`video-backend-api` are contract crates. They should stay backend-neutral and
avoid UI/service dependencies.

`video-backend-api` owns the video backend startup/resource-provider contract:
`VideoBackendFactory`, `StartedVideoBackend`, `PresentFrameResourceProvider` and
the cloneable provider handle used by render lease accounting. It depends on
`video-core`, but must not depend on `player-core`, VA-API, WGPU or renderer
materialization code.

`source-core` owns byte access. It does not know YouTube, containers, codecs,
renderer or player state.

`service-youtube` owns YouTube/yt-dlp details. It returns already opened streaming
media/demuxer objects to the shell and does not contain UI/render/player state.
It also exposes capability-aware candidate metadata for a future startup path
where `capability-core` selects before the demuxer is opened.

`video-vaapi` owns VA-API decode, DMA-BUF export/import and texture pool lifetime.
Its `VaapiWgpuVideoBackendFactory` implements the `video-backend-api`
`VideoBackendFactory` boundary. The concrete backend crate depends on
`video-backend-api`, not on `player-core`.

`render-wgpu` owns WGPU resources, materialization-facing WGPU texture view
types and shader paths. It consumes renderer-neutral metadata plus WGPU texture
views and does not call demux/source/player APIs. The crate still also contains
egui/winit shell composition and a `video-vulkan` reference dependency.

## Направление зависимостей

Фактические direct dependency boundaries после refactor PR:

```text
app-egui -> player-core/service-youtube/desktop-integration
app-egui -> symphonia-demux/audio/video-vaapi/render-wgpu/source-core
app-egui -> media-core/codec-core/capability-core/video-core/render-core/rustiplayer-config
app-egui -> wgpu/winit/egui/egui-winit
player-core -> media-core/codec-core/capability-core/video-core/video-backend-api/rustiplayer-config/audio-core
desktop-integration -> player-core/media-core
service-youtube -> source-core/symphonia-demux/rustiplayer-config/capability-core/codec-core
source-core -> rustiplayer-config
symphonia-demux -> media-core/codec-core/source-core
webm-demux -> symphonia-demux
audio -> audio-core
capability-core -> codec-core/render-core
video-core -> media-core/codec-core
video-backend-api -> video-core
render-core -> codec-core
video-vaapi -> video-backend-api/video-core/media-core/codec-core/capability-core/wgpu
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
  app-egui -> symphonia-demux
  app-egui -> video-vaapi
  player-core -> video-backend-api
  video-vaapi -> video-backend-api
  reverse video-vaapi/player-core edge closed
  player-core -> wgpu closed
  render-wgpu -> egui/egui-wgpu/winit/video-vulkan remains temporary
```

Закрытые связи `player-core -> symphonia-demux/webm-demux` и `player-core -> video-vaapi` не
должны возвращаться. Обратная direct dependency от `video-vaapi` к `player-core`
тоже не должна возвращаться: backend startup/resource-provider boundary живёт в
`video-backend-api`. Текущий production path всё ещё WGPU/VA-API specific,
потому что concrete factory и WGPU materialization собираются в shell/render
composition layer-е.

## Особые случаи

- `desktop-integration` depends on `player-core` and `media-core`, because it is
  an adapter from desktop protocols to public player contracts.
- `video-vaapi::VaapiWgpuVideoBackendFactory` is the production backend factory.
  The deprecated `WgpuVideoBackendFactory` alias now lives in `video-vaapi`, not
  in `player-core`.
- `video-backend-api` owns `VideoBackendFactory`, `StartedVideoBackend` and the
  renderer-neutral resource-provider handle used by `player-core`.
- WGPU texture-view materialization stays in `app-egui`/`render-wgpu`; `player-core`
  exposes opaque frame descriptors and resource lookup/release accounting only.
- `video-vulkan` remains in the workspace but is not the production decode path
  used by `PlayerWorker`; `render-wgpu` still depends on it as reference debt.
- Миграция Symphonia закрыла активный долг локального fork-а: workspace
  использует upstream `symphonia = 0.6` из Cargo, а устаревшие локальные каталоги
  патчей Symphonia удалены. Оставшиеся локальные dependency patches - это
  compatibility patches `cros-codecs`/`cros-libva`, перечисленные выше.
