# Runtime assets and attribution

These are genuine Fastiplayer captures made on **2026-09-05** on the current
development computer: **AMD Ryzen 7 7840HS / Radeon 780M**, Linux
**7.2.2-1-cachyos**, KDE Plasma Wayland session. The application ran through
**XWayland**, with **FFmpeg software decoding** and **WGPU/Vulkan rendering**.
The player window is **1280×720**. A separate temporary profile used audio volume
**0.0**; audio decoding and output remained active (48 kHz, six-channel decoded
AAC; the default device initialized stereo F32 output).

Source revision: [`fab01307b18c824071db55be3659b80f3178e57f`](https://github.com/Bogdan7c/fastiplayer/commit/fab01307b18c824071db55be3659b80f3178e57f).
Build: `cargo build -p app-egui --release --locked`, Rust **1.96.0**.
Package versions remain **0.1.0-alpha.1**. The UI correction in this revision
limits the initial instruction to Idle; pause and other active-media states
preserve the picture without that instruction.

## Published captures

- `fastiplayer-playback.png`: actual local playback with controls visible.
- `fastiplayer-queue.png`: actual queue sidebar with two authorized excerpts.
- `fastiplayer-settings.png`: actual audio settings sidebar during playback.
- `fastiplayer-demo.mp4`: **39 seconds**, H.264/yuv420p, **1280×816**, 30 fps
  screen recording with a 96-pixel English caption band below the player window.
  This capture rate is an encoding choice, not a claim about delivered playback FPS.

PNG captures come directly from FFmpeg X11 window capture with the pointer
excluded; the player UI and movie frames are not retouched. The MP4 records only
the player window, with the pointer included. The system file picker is outside
that capture and this omission is captioned. The recording is continuous; the
only presentation changes are compression and the added caption/attribution band.
No microphone, desktop audio, private media, personal paths, configuration files,
or other desktop windows are included. The [demo page](../demo.md) provides a
text transcript and distinguishes the demonstrated actions from future features.

These assets demonstrate the current application. They are **not performance
measurements**, and are **not the historical ThinkPad T480s captures**. The
[T480s report](../benchmarks/thinkpad-t480s.md) and
[tagged asset directory](https://github.com/Bogdan7c/fastiplayer/tree/v0.1.0-alpha.1/docs/assets)
preserve that earlier evidence with its own provenance. The previous development
screenshot remains in Git history at
[`b575045f`](https://github.com/Bogdan7c/fastiplayer/tree/b575045fd905e46baea1b5534a2eafda10ddb39d/docs/assets).

## Film source and transformations

Movie imagery is from **Big Buck Bunny**:
**© 2008 Blender Foundation / www.bigbuckbunny.org**.
The film is licensed under [Creative Commons Attribution 3.0](https://creativecommons.org/licenses/by/3.0/),
as stated on the [Blender Foundation project page](https://peach.blender.org/about/).
These captures do not imply endorsement by the Blender Foundation.

Source: the official
[`big_buck_bunny_720p_h264.mov.zip` archive](https://download.blender.org/peach/bigbuckbunny_movies/big_buck_bunny_720p_h264.mov.zip).
The extracted film has H.264 **1280×720 at 24 fps** video and six-channel AAC audio.
For this demonstration, two local MP4 excerpts were made with FFmpeg stream copy:
`-ss 15 -t 120` and `-ss 180 -t 90`, mapping the first video and first audio streams
with `-map 0:v:0 -map 0:a:0 -c copy -movflags +faststart`. Seeking occurred before
input; boundaries follow the source's keyframe/timestamp behavior. The video and
audio were not re-encoded for these playback fixtures. Other source tracks were
not selected. The original MOV was rejected by this build's container-opening
path; this demo establishes playback of the derived MP4s only.

The screenshot and video imagery combine those film excerpts with the running
player. The published screen recording is re-encoded and adds English captions
and attribution. The film source files and full downloaded archive are not
bundled in this repository.

## Logo

[`LOGO.png`](../../LOGO.png) is the unchanged original sign.
`fastiplayer-mark.svg` embeds those exact PNG bytes on a permanent dark background
with padding. It neither redraws nor recolors the sign. This presentation keeps
the white mark visible in both light and dark GitHub themes.

## SHA-256

| Artifact | SHA-256 |
| --- | --- |
| Playback screenshot | `3332caf6f65891024dc19004b03f1929962ef67ab4c8bbd659101f69b68faa2f` |
| Queue screenshot | `c2799507fd664cff69c68db4f187de84540e44dacd260e37f0c1e1d6110389e5` |
| Settings screenshot | `d8bf73dc1f5be4746ebb0fa61c8fc22e5bb22f1bd131ad1b4c1fcba8b6f47888` |
| Captioned MP4 | `ac0dbef0fb79079ce92ff6b901a7896195ef28ed67f7e9419c446362c44d6935` |
| Logo presentation | `5e30f2766b499d56b4c22e56869bff21af7aebb9c6ac3333db2eb13eecc1d9a6` |
| Original logo | `a97b93fd3cebed1911194f6ec45d3f13ed5d0cef6c4f1d9d014e60c4e1b4c3f8` |
| Capture executable | `6534a8452d1f9d83c1cc8a8e0205f50a2efcde11189ac32c9049f3b4c2a5f226` |
| Extracted official MOV | `45c8bafeb9a53df7f491198d2e71529701bcf1cd51805782089fac1d32869f9b` |
| Opening excerpt MP4 | `560fcac63e08bd6f4d628725e27989f73acbf9f18f071ad55731a005612511d6` |
| Later excerpt MP4 | `73031919c1f7ce5b7643d144f9cdcf953988b0241ab3081386e429eea9127fa5` |
