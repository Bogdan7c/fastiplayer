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
  Vulkan-first, advanced UI, Phase 10 HDR-to-SDR, future DX12/Metal

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

enum P010RenderReadiness {
    Unavailable,
    ZeroCopyBoundaryVerified,
    Renderable,
}

enum P010StorageLayout {
    BaselineSeparateLayer,
    CompatibilityComposed,
}

enum HdrOutputMode {
    SdrBt709Only,
}

struct RenderCapabilities {
    backend: RenderBackendKind,
    display_name: String,
    supports_hdr_to_sdr: bool,
    supports_native_hdr_output: bool,
    p010_render_readiness: P010RenderReadiness,
    supported_p010_storage_layouts: Vec<P010StorageLayout>,
    supported_hdr_to_sdr_operators: Vec<HdrToneMappingOperator>,
    hdr_output_mode: HdrOutputMode,
    supported_frame_formats: Vec<VideoFrameFormat>,
    max_texture_size: Option<u32>,
    advanced_ui: bool,
    ui_composition_mode: UiCompositionMode,
    present_timing_metrics: bool,
}

enum SwapchainTransferMode {
    PreserveCurrentUnorm,
    SrgbRenderTarget,
    ExplicitShaderOetf,
}

struct ColorPipelineSettings {
    adjustment: ColorAdjustment,
    tone_mapping: ToneMappingMode,
    swapchain_transfer: SwapchainTransferMode,
}

struct ColorAdjustment {
    brightness: f32,
    contrast: f32,
    saturation: f32,
    exposure: f32,
    rgb_gain: [f32; 3],
    rgb_offset: [f32; 3],
}

struct HdrToSdrSettings {
    enabled: bool,
    operator: HdrToneMappingOperator,
    output_mode: HdrOutputMode,
    sdr_reference_white_nits: f32,
    hdr_reference_peak_nits: f32,
}
```

## Vulkan profile

Config profile `vulkan` - основной режим.

Ожидания:

- advanced egui UI;
- NV12 production shader path;
- P010 zero-copy boundary diagnostics and Phase 10 P010/HDR shader path;
- Phase 10 HDR-to-SDR tone mapping;
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

Timeline/seek UI не должен напрямую мутировать позицию playback. Ползунок
timeline читает typed `PlayerSnapshot.timeline`, а действия отправляет как
`BeginScrub`, `UpdateScrub(SeekRequest)` и `EndScrub { CommitLatest }`.
Обычные hotkeys используют `SeekTarget::Relative(Duration)` с шагами из
`player.seek.hotkey_small_step_secs` и `player.seek.hotkey_large_step_secs`.
Если `timeline.seekable == false`, UI показывает disabled state и diagnostics из
`timeline.not_seekable_reason`, но не изобретает собственную business-причину.

`ui.skin = "minimal"` в schema v2 фиксирует первый production skin. Unknown skin
id является config error, пока validation явно не вводит mapping на default.

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

MPRIS seek/progress должен использовать те же neutral timeline contracts:
`MediaTime`, `MediaDuration`, `SeekTarget` и `TimelineSnapshot`. Linux MPRIS -
первый concrete adapter, а не часть core timeline model.

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

## Phase 8.5 SDR color pipeline prep

Phase 8.5 исторически подготовил renderer к HDR, но сам HDR не реализовал. Текущий статус после Phase 10: P010/HDR renderer реализован отдельно, а ниже перечисленные SDR решения остаются действующими ограничителями для `nv12_to_rgba.wgsl`.

Принятые решения:

- swapchain transfer описывается enum-ом; default - `PreserveCurrentUnorm`, чтобы не менять текущий SDR result;
- реальные color metadata собираются layered-моделью с origin/confidence;
- tone mapping presets остаются typed future contract; Phase 10 добавляет только фиксированный `bt2446_c` config без пользовательских preset controls;
- SDR/RGB adjustments добавляются в settings/config с identity defaults;
- NV12 BT.2020 SDR сейчас отображается как fallback path в SDR BT.709 diagnostics, настоящий gamut mapping добавляется позже; P010 BT.2020 SDR rejected до отдельного wide-gamut SDR решения.

### Swapchain transfer

`Unorm` и `UnormSrgb` нельзя выбирать как взаимозаменяемые форматы. При `UnormSrgb` GPU применяет sRGB conversion при записи в render target, а при `Unorm` shader output должен уже быть display-referred. Поэтому текущий порядок выбора surface format фиксируется как осознанный режим `PreserveCurrentUnorm`.

Future modes:

- `SrgbRenderTarget` - shader отдаёт linear SDR, target делает sRGB encode;
- `ExplicitShaderOetf` - shader явно применяет output transfer и пишет в `Unorm`;
- HDR/native output modes - только после отдельного platform-specific решения.

### Active color path

Renderer должен уметь вернуть diagnostics вроде:

```text
NV12 8-bit BT.709 limited -> SDR BT.709 preserve-current-unorm
NV12 8-bit BT.2020 limited -> SDR BT.709 fallback preserve-current-unorm
```

BT.2020 SDR fallback не означает wide-gamut output support. Это только честная диагностика временного поведения до gamut mapping.

### Shader boundary

`nv12_to_rgba.wgsl` остаётся NV12 SDR shader path. Он получает range normalization, YUV->RGB matrix, SDR adjustments и debug mode через uniforms. Shader не должен превращаться в универсальный HDR-комбайн; Phase 10 P010/HDR получает отдельный renderer path и отдельный shader.

## Phase 9/10 P010 policy

Phase 9 проверял только P010 zero-copy render boundary. Это не было production rendering path. Phase 10 перевёл поддержанный P010/HDR path в production `Renderable` состояние.

```text
Unavailable -> P010 path недоступен
ZeroCopyBoundaryVerified -> P010 DMA-BUF импорт и plane views доказаны ручным dev-тестом
Renderable -> renderer имеет production P010 shader path
```

Production HDR playback разрешается только при `Renderable` и `supports_hdr_to_sdr = true`.

Phase 10 переводит P010/HDR path в `Renderable` через отдельный shader `p010_bt2446c_to_sdr.wgsl`, BT.2446 Method C и `ExplicitShaderOetf`. Это текущий WGPU production path при passing capability intersection; native HDR output остаётся `false`.
