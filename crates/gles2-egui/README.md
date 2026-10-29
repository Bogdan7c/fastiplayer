# gles2-egui

An OpenGL ES 2.0 renderer for [egui](https://github.com/emilk/egui) that runs on graphics
hardware the Vulkan and GLES 3 backends cannot reach. Fastiplayer uses it to draw its whole
interface on a 2010 ThinkPad T410 (Intel Ironlake, no Vulkan, no GLES 3), and it is written to be
plugged into any egui application.

It is part of the [Fastiplayer](../../README.md) workspace and is not published on crates.io yet:
use it as a git or path dependency. The API may still change.

## What it gives you

- **An EGL context on your winit window** — native Wayland (`wl_egl_window`) or X11/XWayland
  (Xlib), chosen from the window's raw handles — or a window-less surfaceless context for pixel
  tests (`Gles2Context::offscreen`).
- **Driver capabilities measured at startup**, not guessed from the GPU name:
  `Gles2Capabilities::tiers()` reports what the driver can do and why a level is missing.
- **An egui painter** (`EguiPainter`) with the egui contract: premultiplied alpha,
  gamma-space blending, `CLAMP_TO_EDGE`, the same dithering as `egui-wgpu`. Pixel parity with
  `egui-wgpu` is tested in the Fastiplayer workspace (max difference 1/255).
- **A frame compositor** (`FrameCompositor`): background → your underlay (for example video) →
  your overlays (for example subtitles) → egui → present. It can re-show the previous egui frame
  without a new egui pass (`UiContent::ReusePrevious`) and redraw only the damaged area when the
  driver supports `EGL_EXT_buffer_age`.
- **GPU fences and a completion queue** (`GpuFence`, `GpuCompletionQueue`) to give buffers back
  exactly when the GPU has finished reading them.
- **Typed errors instead of panics**: shader compile failures, driver refusals and context loss
  (`ContextLoss`) are values you can handle.

Only two thin "wires" to the driver come from outside: [`glow`](https://crates.io/crates/glow)
(the GL function table) and [`khronos-egl`](https://crates.io/crates/khronos-egl) (EGL,
`libEGL.so.1` loaded at run time), plus `wayland-sys` for `wl_egl_window`. Painting, shaders and
frame logic are this crate's own code. The crate does not depend on any Fastiplayer player
crate; a guardrail script in the workspace enforces that.

## Requirements

| Requirement | Why |
| --- | --- |
| Linux with Mesa (or another EGL driver) | the context is created through EGL |
| EGL 1.5 | platform displays for Wayland/X11; EGL 1.4 gets a typed `EglVersionUnsupported` |
| OpenGL ES 2.0 | the whole renderer is GLSL ES 1.00 (`#version 100`) |
| Opaque windows | the window is always drawn opaque; there is no transparent-window mode |

Drivers that answer a GLES 2 request with an ES 3.x context (radeonsi, llvmpipe) are fine: the
crate uses only ES 2.0 plus extensions, so it behaves the same there as on real ES 2.0 hardware.

### Capability tiers

| Tier | Driver needs | Unlocks |
| --- | --- | --- |
| Baseline (required) | GLES 2.0, EGL 1.5 | egui, 8-bit video, overlays |
| Precision | `highp` floats in fragment shaders | 10/12-bit video and HDR→SDR in `gles2-video` |
| Zero-copy | `EGL_EXT_image_dma_buf_import(_modifiers)`, `GL_OES_EGL_image`, DRM `R8`/`GR88` as `sampler2D` | importing DMA-BUF frames without a copy |
| Zero-copy 10-bit | the above + precision + `ABGR8888` | reported only; Fastiplayer does not use it on GLES2 |
| Conveniences | `GL_EXT_unpack_subimage`, `GL_OES_element_index_uint`, `GL_OES_vertex_array_object`, `EGL_KHR_fence_sync`, `EGL_EXT_buffer_age`, `EGL_KHR/EXT_swap_buffers_with_damage` | fewer GL calls; each one has a fallback (16-bit index batches, attribute setup per draw, `glFinish` instead of a fence, full redraws) |

## Minimal use

A complete, runnable program — winit window, egui input through `egui-winit`, egui on top of a
video layer fed from CPU memory — is
[`crates/gles2-video/examples/egui_video_window.rs`](../gles2-video/examples/egui_video_window.rs).
CI compiles it on every push. The core of a frame looks like this:

```rust
use std::sync::Arc;
use gles2_egui::{
    EguiPainterOptions, FrameCompositor, Gles2Context, NoFrameLayers, PixelExtent,
    PresentationSettings, UiContent, UiFrameOutcome, UiFrameRequest, UiScreen, WindowSource,
};

// Once, after winit created the window (`Arc<winit::window::Window>`).
let size = window.inner_size();
let source: Arc<dyn WindowSource> = window.clone();
let mut context = Gles2Context::for_window(
    source,
    PixelExtent { width: size.width, height: size.height },
    PresentationSettings::default(), // waits for vblank
)?;
let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default())?;

// Every frame, after your egui pass (`egui_ctx.run_ui(..)` + `tessellate(..)`).
let extent = context.extent();
let outcome = compositor.render(
    &mut context,
    UiFrameRequest {
        screen: UiScreen {
            size_in_pixels: [extent.width, extent.height],
            pixels_per_point: window.scale_factor() as f32,
        },
        content: UiContent::Fresh { primitives, textures_delta: output.textures_delta },
        background: [0.0, 0.0, 0.0, 1.0], // premultiplied RGBA
        layers: &mut NoFrameLayers,       // or a video underlay, see gles2-video
    },
)?;
match outcome {
    UiFrameOutcome::Presented(_) => {}
    UiFrameOutcome::Skipped(_) => {} // zero-sized or detached window: nothing to show
    UiFrameOutcome::Dropped(reason) => { /* e.g. textures out of sync: send a full set */ }
}

// On `WindowEvent::Resized`.
context.resize(PixelExtent { width, height })?;

// On exit: GL objects first, the context last.
compositor.destroy(&context)?;
```

Your own GL drawing goes into a `FrameLayers` implementation: `draw_underlay` runs after the
background fill and before egui, `draw_overlays` runs above the underlay and below egui. Both
get an `ActiveFrame` with the `glow::Context`.

## Known limitations

- **One EGL display per native display, shared inside this crate only.** Mesa hands out the same
  `EGLDisplay` for the same Wayland or X11 connection. This crate counts its own contexts and
  calls `eglTerminate` after the last one is gone. It cannot see other EGL users in the same
  process, such as the GL backend of `wgpu` or an mpv render context: when either of them
  terminates the display, the other loses it. Do not mix another EGL user on the same native
  display in one process for now. A "never terminate" mode is in the project backlog.
- **X11 through Xlib handles only.** An XCB-only window handle returns `UnsupportedWindowSystem`
  (winit provides Xlib handles by default).
- **Two presentation modes, no frame-latency control.** `VsyncMode::WaitForVblank` (default) and
  `VsyncMode::Immediate` (`eglSwapInterval(0)`); there is no mailbox mode.
- **GPU reset recovery is reported, not exercised.** Context loss is detected and reported as
  `ContextLoss`; a real GPU reset has not been triggered in tests.

Hardware this has been verified on and the measured results are in the
[GLES2 compatibility notes](../../docs/render/gles2-compatibility.md).

## Testing

Pixel tests render into an offscreen surfaceless context and read pixels back. Without EGL they
are skipped with a printed reason; set `GLES2_EGUI_REQUIRE_EGL=1` to make a missing EGL a
failure (CI does this on Mesa llvmpipe). Two more examples live here:
`clear_window --scripted-check` (window, detach/reattach, vsync change, resize and fence) and
`egui_window --bench` (painter timings on real hardware).

## License

MIT, like the rest of Fastiplayer's first-party code (see [LICENSE](../../LICENSE)).
