# 06. Rendering, UI and Platform

## Platform priority

Порядок платформ:

1. Linux
2. Windows
3. macOS

Linux - основная платформа. Wayland является primary target. X11 нужен как fallback, особенно для старых устройств и будущего OpenGL ES renderer.

## Primary renderer

Primary renderer: `render-wgpu`.

На Linux основная цель - Vulkan через `wgpu`.

`wgpu` покрывает Vulkan/Metal/DX12/OpenGL/WebGPU/WebGL2 на уровне абстракции, но OpenGL ES 2.0 legacy path не стоит считать полноценной частью primary renderer. Для старых устройств нужен отдельный `render-gles`.

## Render backend split

```text
render-core
  common contracts

render-wgpu
  Vulkan-first, advanced UI, HDR-to-SDR, future DX12/Metal

render-gles
  future X11/GLES2 fallback, SDR 8-bit NV12 only
```

## render-core contracts

`render-core` должен описывать:

- render backend id;
- render capabilities;
- supported input frame formats;
- color conversion support;
- HDR/tone mapping support;
- UI composition mode;
- present timing metrics.

Пример:

```rust
enum RenderBackendKind {
    Wgpu,
    OpenGles,
}

enum VideoFrameFormat {
    Nv12,
    P010,
    Rgba8,
}

struct RenderCapabilities {
    backend: RenderBackendKind,
    supports_hdr_to_sdr: bool,
    supports_native_hdr_output: bool,
    supported_frame_formats: Vec<VideoFrameFormat>,
    max_texture_size: Option<u32>,
    advanced_ui: bool,
}
```

## Vulkan profile

Config profile `vulkan` - основной режим.

Ожидания:

- advanced egui UI;
- NV12/P010 shader path;
- HDR-to-SDR tone mapping;
- frame pacing diagnostics;
- GPU resource telemetry;
- future compute-assisted color pipeline, если потребуется.

## OpenGL ES profile

Config profile `opengles` - будущий fallback.

Ожидания:

- X11 fallback;
- OpenGL ES 2.0 compatibility;
- SDR 8-bit NV12;
- простая графика;
- минимальный overlay;
- приоритет скорости и совместимости.

Не требуется:

- advanced UI parity;
- true HDR;
- complex effects;
- heavy telemetry overlay.

## UI architecture

UI остается на egui.

Целевой принцип:

```text
egui input -> PlayerCommand -> PlayerSession -> PlayerSnapshot -> egui view
```

UI не должен хранить player business state. Он может хранить только UI-local state:

- открытые панели;
- selected tab;
- transient dialog state;
- layout preferences;
- search text.

## UI modules

В `app-egui` нужно постепенно выделить:

```text
src/
  main.rs          - winit ApplicationHandler
  shell.rs         - связывает app, player, renderer
  ui/
    mod.rs
    player_controls.rs
    telemetry_panel.rs
    media_info.rs
    settings.rs
    capability_view.rs
    errors.rs
  input.rs         - hotkeys -> PlayerCommand
```

## Desktop integration

Нужен отдельный crate `desktop-integration`.

Linux priority:

- MPRIS D-Bus;
- KDE media widget playback controls;
- metadata export;
- play/pause/seek/next/previous commands;
- inhibit sleep/screensaver during playback;
- optional notifications.

`desktop-integration` должен работать через `PlayerCommand` и `PlayerSnapshot`, а не напрямую через player internals.

## Windowing

`winit 0.30` ApplicationHandler остается правильной базой.

Правило:

- `ApplicationHandler` управляет lifecycle;
- `resumed/suspended` создают и освобождают window-bound resources;
- `window_event` переводит input в команды;
- render path вызывает renderer;
- player business logic живет вне ApplicationHandler.

## Device loss and recovery

Renderer должен уметь:

- обработать surface lost/outdated;
- пересоздать surface config;
- сообщить player/UI о render backend error;
- не терять media state при пересоздании GPU resources, если возможно.

## Color pipeline

Color pipeline должен быть явным:

```text
decoded frame metadata
  -> color range normalization
  -> YUV to RGB
  -> HDR-to-SDR tone mapping if needed
  -> output transfer/color space
  -> swapchain
```

Нельзя считать все видео BT.709 limited 8-bit. Это приемлемо для MVP, но не для целевой архитектуры.

