# OpenGL ES 2.0 renderer: compatibility and limits

Fastiplayer has two renderers. On a GPU with a working hardware Vulkan driver it draws through
**wgpu/Vulkan**. Everywhere else it uses its own **OpenGL ES 2.0** renderer ("GLES2"), written
for laptops and desktops that Vulkan never reached: Intel GPUs before Gen7 (Ironlake/Sandy
Bridge), old AMD and NVIDIA cards on Mesa, and machines where only a software Vulkan
(lavapipe) exists. This page lists what the GLES2 renderer needs, what it was verified on, what
it does without a CPU copy, and where it stops.

Status: released in the development tree for roadmap item 6
([issue #6](https://github.com/Bogdan7c/fastiplayer/issues/6)). Measurements against mpv and VLC
on a 2010 ThinkPad T410 are in the [T410 report](../benchmarks/t410-gles2-2026-10-29.md).

## How the renderer is chosen

The **Renderer** setting has three values: *Automatic* (default), *Vulkan* and *OpenGL ES*. In
`~/.config/fastiplayer/config.toml` it is:

```toml
[render]
profile = "auto"   # or "vulkan", or "opengles"
```

A change applies after a restart; the settings window shows "Applies after restart" and a
**Restart now** button.

- **Automatic** picks Vulkan when there is a hardware Vulkan adapter with the extensions the
  player needs. It picks OpenGL ES 2.0 when there is no Vulkan device, only a CPU Vulkan
  (lavapipe/llvmpipe), or wgpu could not start. This choice is silent; the log and the telemetry
  panel show it.
- **A crash during startup is remembered.** If a renderer did not reach its first frame last
  time, the next start skips it and shows a notification.
- **Explicit Vulkan on a machine where Vulkan cannot work** (no adapter, only lavapipe, or
  creation failed) starts on OpenGL ES 2.0 for this run with a notification. The setting stays
  Vulkan, so the next start tries Vulkan again.
- **On old hardware choose the *Dynamic* decoding mode** (`[video] preferred_backend =
  "dynamic"`). It starts on hardware decoding and switches the current file to software once if
  the hardware decoder falls behind. The settings window suggests it when OpenGL ES 2.0 is
  active.

## What the driver must offer

Levels are measured from the driver at startup, never guessed from the GPU name. The table is
the same as in the [`gles2-egui` README](../../crates/gles2-egui/README.md#capability-tiers).

| Level | Driver needs | What it unlocks in Fastiplayer | Without it |
| --- | --- | --- | --- |
| Baseline | OpenGL ES 2.0, EGL 1.5 | the interface, 8-bit video, subtitles | the renderer is not offered; on EGL 1.4 the error is `EglVersionUnsupported` |
| Precision | `highp` floats in fragment shaders | 10/12-bit video, HDR-to-SDR | 10/12-bit files and HDR are not offered on this renderer |
| Zero-copy | `EGL_EXT_image_dma_buf_import` (+ modifiers), `GL_OES_EGL_image`, `R8`/`GR88` DMA-BUF textures | VA-API hardware decoding; software frames without a CPU copy | hardware decoding is not offered for GLES2; software frames take one copy |
| Conveniences | `GL_EXT_unpack_subimage`, `GL_OES_element_index_uint`, `GL_OES_vertex_array_object`, `EGL_KHR_fence_sync`, `EGL_EXT_buffer_age`, swap with damage | fewer GL calls, partial redraws | each has a fallback; nothing stops working |

Drivers that return an ES 3.x context for a GLES 2 request (radeonsi, llvmpipe) are used the
same way: only ES 2.0 plus extensions.

## Verified hardware and drivers

| Machine | GPU / driver | Result |
| --- | --- | --- |
| ThinkPad T410 (2010), Core i5 M560 | Intel HD Graphics (Ironlake, Gen5), Mesa 26.2.4 **crocus**, GLES 2.0 / GLSL ES 1.0.16, EGL 1.5, no Vulkan device; VA-API `intel-vaapi-driver` (i965) 2.4.5 | **Automatic → OpenGL ES 2.0.** All levels present. Hardware H.264 zero-copy, software decoding without a CPU copy (needs a CPU cache flush of visible rows, see below). Qualified against mpv and VLC 3 on native Wayland and XWayland ([report](../benchmarks/t410-gles2-2026-10-29.md)). |
| Ryzen 7 7840HS laptop | Radeon 780M, Mesa 26.2.5 **radeonsi** (ES 3.2 context for a GLES 2 request) | Automatic → Vulkan. OpenGL ES 2.0 forced for qualification against mpv and the wgpu renderer: no frame loss on any scene ([report](../benchmarks/local-gles2-2026-10-26.md)). |
| ThinkPad T480s (2018), Core i5-8350U | Intel UHD 620, Mesa 26.2.4 (ANV Vulkan / iris), VA-API iHD | Automatic → Vulkan. OpenGL ES 2.0 forced for AV1 playback checks: no frame loss up to 1080p60, CPU cache flush not needed. |
| CI (GitHub Actions, Ubuntu) | Mesa **llvmpipe**, surfaceless EGL | Every pixel test of the renderer runs here on each push; a missing EGL fails the run instead of skipping it. |

Pixel output matches the Vulkan renderer: the interface, 8-bit video, NV12 and subtitles differ
by at most 1/255 per channel; HDR-to-SDR by at most 2/255 (see [HDR](#hdr)).

## Copies per video frame

| Source | Path on GLES2 | CPU copies |
| --- | --- | --- |
| Hardware H.264 (VA-API), 8-bit | VA surface → DMA-BUF → EGLImage | **0** |
| Hardware 10-bit (P010) | not offered on GLES2: such files use software decoding | — |
| Software decoding (FFmpeg, dav1d for AV1), 8-bit and 10/12-bit | decoder writes into a sealed memfd → `/dev/udmabuf` → EGLImage | **0** when the conditions below hold, otherwise **1** |
| Software AV1 with a system FFmpeg (`--features system-ffmpeg`) | dav1d keeps its own buffers | **1** (the bundled FFmpeg carries a small dav1d patch that removes it) |

Software frames go without a copy only when all of these hold; otherwise the player silently
uses the single copy (the driver copies rows into a texture):

- the driver has the zero-copy level;
- `/dev/udmabuf` exists and your user can open it (desktop sessions usually grant this through
  an ACL; the module is `udmabuf`);
- a startup self-check proves that the GPU sees what the CPU wrote. On GPUs without a shared
  CPU/GPU cache (Ironlake) the player then flushes the CPU cache for the visible rows of every
  frame, about 1.2–1.7 ms per 1080p frame on the T410 — still cheaper than the copy (2.4 ms).

The log states the result once per start, for example
`GLES2: software frames are shown without CPU copy (udmabuf), CPU cache flush: visible rows of every frame`
or `GLES2: /dev/udmabuf is not accessible: …`.

## Known limitations

- **No hardware 1080p60 H.264 on Ironlake.** The GPU decodes 1080p at about 39 frames per second
  at best, for any player. In *Dynamic* mode Fastiplayer moves such files to software decoding
  after about 6.5 s and plays them without loss; in *Automatic* or *Hardware* mode they stutter.
- **Hardware decoding on fast playback.** On the T410, hardware 1080p30 cannot keep up at 2× or
  4×; video then falls behind the audio and catches up after returning to 1×. *Dynamic* mode
  avoids this.
- **10-bit hardware decoding is not used on GLES2.** GPUs that decode 10-bit in hardware have
  Vulkan and use the Vulkan renderer; on GLES2, 10-bit plays through software decoding.
- <a id="hdr"></a>**HDR is shown as SDR.** HDR10/HLG are tone-mapped to SDR BT.709 (BT.2446-C). On
  GLES2 the tone curve comes from a 65³ table computed on the CPU, which is up to 3/255 away from
  the exact formula (2/255 from the Vulkan renderer). The first table takes 41–50 ms of CPU on
  the T410 and is built in the background. There is no native HDR output.
- **MPEG-2 video is not supported** by the player at all (any renderer): such files play audio
  only. Ironlake could decode MPEG-2 in hardware; this is a backlog item.
- **12-bit and 4:2:2/4:4:4 files are not opened** (HEVC Main12/4:2:2/4:4:4, VP9 profiles 1 and 3
  and 12-bit, AV1 High 4:4:4), on any renderer. The decoder and both renderers can already show
  most of these layouts; an older gate in the player's codec layer refuses them. This is a
  backlog item.
- **EGL 1.5 is required.** Mesa has had it since 2015; EGL 1.4-only drivers are not supported.
- **The window is always opaque, without rounded corners**, in both renderers. On Ironlake an
  alpha window cannot go to the display plane, and the compositor would copy every frame.
- **Old GPUs can hang under load.** The Ironlake GPU of the test T410 occasionally hangs inside
  the driver (`GPU HANG` in `journalctl -k`) and is reset by the kernel. During qualification
  this happened in other programs too (the KDE panel, VLC), and once it leaves the GPU unreliable
  until a reboot. If you see repeated GPU hangs, please report them with the kernel log.

## Send a report from your machine

Reports from real old hardware are the most useful thing for this renderer. Please open an
[issue](https://github.com/Bogdan7c/fastiplayer/issues/new?template=bug_report.yml) or a
[Discussions](https://github.com/Bogdan7c/fastiplayer/discussions) thread with:

1. Your laptop or GPU model and distribution.
2. The output of these commands (`eglinfo` and `es2_info` come with your distribution's Mesa
   utilities, for example `mesa-utils`; `vainfo` with `libva-utils`):

   ```sh
   eglinfo -B
   es2_info | head -n 20
   vainfo
   ls -l /dev/udmabuf
   uname -r
   ```

3. The renderer lines of a short run of the player:

   ```sh
   RUST_LOG=info ./target/release/fastiplayer /path/to/video.mp4 2>&1 \
     | grep -E "Renderer selected|GLES2|Selected video pipeline|Dynamic decode|GPU" | head -n 30
   ```

   They name the chosen renderer and why (`Renderer selected renderer="gles2" why=…`), the
   driver and the levels it reached (`GLES2 renderer 1 is ready: …`), the decode path, and
   whether software frames go without a copy.
4. What you played (codec, resolution, frame rate) and what you saw: smooth, dropped frames,
   colors, crashes. If something froze, add `journalctl -k -b | grep -i -E "hang|i915|amdgpu|radeon|nouveau"`.

Please do not attach video files; describe them or link a public source.
