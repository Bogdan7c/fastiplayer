# Runtime assets and attribution

The [72-second trailer](https://www.youtube.com/watch?v=eMfzBhpSF8M) and these genuine Fastiplayer captures were made on **2026-09-06** on the current development computer: **AMD Ryzen 7 7840HS / Radeon 780M**, Linux **7.2.2-1-cachyos**, KDE Plasma Wayland. The player used **VA-API H.264 hardware decoding**, NV12 DMA-BUF video frames, and **WGPU/Vulkan rendering** through XWayland.

Source revision: [`0638b476bba429e4f80d617c3a5e73319cd80984`](https://github.com/Bogdan7c/fastiplayer/commit/0638b476bba429e4f80d617c3a5e73319cd80984).
Build: `cargo build -p app-egui --release --locked`, default features, Rust **1.96.0**. Package versions remain **0.1.0-alpha.1**. The capture uses a separate temporary application profile, preserving the user's configuration.

## Published assets

- `fastiplayer-trailer-cover.png`: a genuine final-card frame at 01:10.5 from the trailer, including native Resolve titles over the captured application.
- `fastiplayer-playback.png`: playback at 00:34 of the Sintel Trailer with the window maximized and no panels open (2026-10-11 capture, see below).
- `fastiplayer-main.png`: the original 2026-09-06 playback screenshot, kept unchanged at the path used in the existing alpha release notes.
- `fastiplayer-queue.png`: playback at 00:29 with the queue sidebar open, holding the Sintel Trailer and Big Buck Bunny (2026-10-11 capture, see below).
- `fastiplayer-subtitles.png`: fullscreen Sintel with an English subtitle in the default style above the playback controls (2026-10-19 capture, see below).
- `fastiplayer-t480s-av1-4k60.png`: Big Buck Bunny AV1 4K60 playing on the ThinkPad T480s of the October CPU-parity report (2026-10-23 capture, see below).
- `fastiplayer-settings.png`: video color settings during playback, with saturation deliberately raised to 2.45 to show its effect.
- `fastiplayer-t410-gles2-1080p60.png`: Big Buck Bunny 1080p60 playing through the OpenGL ES 2.0 renderer on a 2010 ThinkPad T410 (2026-10-28 capture, see below).

Screenshots are **1920×1080 PNG** frames extracted from the real OBS recordings, with the recorded cursor included. The cover is extracted from the finished trailer. The player UI and movie frames are not retouched. The [demo page](../demo.md) supplies timecodes and distinguishes the demonstrated interactions from future features.

### 2026-10-11 screenshots

`fastiplayer-playback.png` and `fastiplayer-queue.png` were re-captured on **2026-10-11** on the same computer (Linux **7.2.9-2-cachyos**) to show the current interface. Source revision [`727d5346`](https://github.com/Bogdan7c/fastiplayer/commit/727d5346f687614aaf79567ce4b14ef07af72edf), `cargo build -p app-egui --release --locked`, Rust **1.99.0**, with a separate temporary profile and telemetry hidden. The player ran in an isolated nested KWin Wayland session at 1920×1080 with VA-API hardware decoding and WGPU/Vulkan rendering; frames are KWin screenshots of that session without a cursor, losslessly recompressed. Media: the official Sintel Trailer and Big Buck Bunny, both © copyright Blender Foundation, CC BY 3.0. The UI and movie frames are not retouched.

### 2026-10-19 subtitle screenshot

`fastiplayer-subtitles.png` was captured on **2026-10-19** during the final acceptance of the subtitle engine (issue #1), on the same computer (Linux **7.2.10-1-cachyos**, KDE Plasma Wayland, single 1920×1080 monitor).
- **Build:** source revision [`0c697dcf`](https://github.com/Bogdan7c/fastiplayer/commit/0c697dcf) plus the session 15 fixes committed right after it. Built with `cargo build -p app-egui --release --locked` (Rust **1.99.0**), using a separate temporary profile.
- **Player state:** fullscreen, paused at 00:56 of a cut from the official Sintel 720p release, with the official English SubRip file next to the video. VA-API hardware decoding and WGPU/Vulkan rendering, subtitle style at its defaults, playback controls shown by a mouse move.
- **Capture:** a spectacle full-screen capture without the cursor, losslessly recompressed. The UI, the subtitle and the movie frame are not retouched.
- **Media:** *Sintel* © copyright Blender Foundation, CC BY 3.0. The subtitle text comes from the official Sintel subtitle files.

### 2026-10-23 ThinkPad T480s screenshot

`fastiplayer-t480s-av1-4k60.png` was captured on **2026-10-23** during session 10 of the CPU-parity plan, on the laptop of the [T480s CPU-parity report](../benchmarks/t480s-cpu-parity-2026-10-23.md). Hardware: ThinkPad T480s, Intel Core i5-8350U, UHD Graphics 620. Software: Linux **7.2.9-2-cachyos**, KDE Plasma Wayland, 1920×1080 at 1.25 scale.
- **Build:** source revision `cba51ace`, `cargo build -p app-egui --release --locked` (Rust **1.99.0**), the same binary as the report, with a separate temporary profile (the report's software template).
- **Player state:** native Wayland window at its default size, playing at 00:41 of the Big Buck Bunny AV1 4K60 file from the report. Software AV1 decoding, frames imported into Vulkan without a CPU copy (confirmed by the runtime log), volume 0.
- **Capture:** spectacle active-window capture, 1280×720 logical pixels, without the cursor. The UI and the movie frame are not retouched.
- **Media:** *Big Buck Bunny* © 2008 Blender Foundation / www.bigbuckbunny.org, CC BY 3.0.

### 2026-10-28 ThinkPad T410 screenshot

`fastiplayer-t410-gles2-1080p60.png` was captured on **2026-10-28** during the publication session of the OpenGL ES 2.0 renderer (issue #6), on the laptop of the [T410 report](../benchmarks/t410-gles2-2026-10-29.md). Hardware: ThinkPad T410 (2010), Intel Core i5 M560, Intel HD Graphics (Ironlake), no Vulkan device. Software: Linux **7.2.7-1-cachyos**, Mesa **26.2.4** (crocus), KDE Plasma **6.8.1** Wayland, 1440×900 panel.
- **Build:** source revision `4189ea33` (code identical to `eacd5dbb`), `cargo build -p app-egui --release --locked` (Rust **1.99.0**), the binary of the report, with a separate temporary profile: renderer *Automatic*, decoding *Dynamic*, volume 0.
- **Player state:** native Wayland window at its default size, 00:18 into the clip. The log shows the automatic choice (`Renderer selected renderer="gles2" why=OpenGL ES 2.0 — Auto: no hardware Vulkan adapter`); the file started on VA-API hardware decoding, and after 6 s *Dynamic* moved it to software decoding with frames shown without a CPU copy (the GPU cannot decode 1080p60 H.264 in real time).
- **Media:** a 30-second cut from 01:30 of the official *Big Buck Bunny* 4K60 release, re-encoded to H.264 High 1920×1080 60 fps, 12 Mbit/s, AAC stereo (SHA-256 `f9660d8d…b2d7c`). The clip is kept outside Git.
- **Capture:** spectacle active-window capture, 1280×720, without the cursor, saved as opaque RGB. The UI and the movie frame are not retouched.
- **Media license:** *Big Buck Bunny* © 2008 Blender Foundation / www.bigbuckbunny.org, CC BY 3.0.

Video is hosted on YouTube. Large recordings, film media, the DNxHR master, and the Resolve `.drp` project are kept outside Git. The previous silent 39-second MP4 is superseded by this trailer; its historical bytes remain in Git history. Existing releases and tags are unchanged.

These captures are **not performance measurements**. Apart from the 2026-10-23 screenshot above, they are **not ThinkPad T480s captures**. The [T480s report](../benchmarks/thinkpad-t480s.md) and [tagged asset directory](https://github.com/Bogdan7c/fastiplayer/tree/v0.1.0-alpha.1/docs/assets) retain their own provenance. The September 5 presentation and Big Buck Bunny attribution remain in [the preceding asset documentation](https://github.com/Bogdan7c/fastiplayer/blob/0638b476bba429e4f80d617c3a5e73319cd80984/docs/assets/README.md).

## Recording, editing, and verification

| Stage | Actual parameters |
| --- | --- |
| OBS Studio | Flatpak **32.2.2**; separate profile and scene collection; **1920×1080, 60 FPS**; PipeWire capture of the player window only. |
| Recording video | Hybrid MOV; **H.264 High**, x264 CRF 16, veryfast, 1-second keyframe interval; NV12 / Rec.709 limited range. |
| Recording audio | **PCM signed 24-bit little-endian, 48 kHz, stereo**, track 1; isolated player-output monitor, −3 dB input gain, unmuted; monitoring off; no microphone or duplicate desktop mix. |
| Editing | **DaVinci Resolve Studio 21.0.4**; 1920×1080 / 60 FPS timeline; six continuous real-time source segments, corresponding audio, native Fusion Text+ titles, final card and a short final audio fade. |
| Resolve master | MOV, **DNxHR HQ**, 8-bit 4:2:2 / Rec.709; PCM 24-bit / 48 kHz stereo; **72.000 seconds, 4,320 video frames**. |
| YouTube upload | MP4, H.264 High / yuv420p / Rec.709, **1920×1080 at 60/1 FPS**; AAC-LC 48 kHz stereo; 72.000 seconds. Final encoding: x264 slow, CRF 18, AAC target 320 kbit/s (measured average about 259 kbit/s). |

The OBS → Resolve chain was tested before principal recording with real video and audio import and a short edited export. Final selected takes reported zero skipped rendering or encoding frames in OBS. Frame timestamps and codecs were checked with `ffprobe`; the film's 24 FPS was not used as proof of 60 FPS UI capture.

One-second interior PCM checks in all six master segments matched their corresponding OBS samples byte for byte at the expected edit positions. Full final-file playback reached EOF without a decoding error. The upload file measured −18.0 dBFS mean and −3.1 dBFS peak with FFmpeg `volumedetect`; real output-device monitoring also confirmed nonzero sound. YouTube playback was checked at **1920×1080@60**, with source audio reaching the output device. These checks are scoped media-verification evidence, not a listening-quality or physical-display benchmark.

The demonstrations are not retimed or frame-interpolated. Color changes come from the application's controls, with no Resolve color grade applied to the player image. Only the authorized source-film audio is used: no added music, narration, or UI effects. Private paths, microphone audio, desktop notifications, configuration, and other application windows are excluded from the published footage.

## Film source and license

Film and included audio: **Sintel © copyright Blender Foundation | durian.blender.org**.

The [official sharing terms](https://durian.blender.org/sharing/) license the Durian project's published content under [Creative Commons Attribution 3.0](https://creativecommons.org/licenses/by/3.0/), permitting reuse, editing, and commercial distribution with attribution. These captures do not imply endorsement by the Blender Foundation. Separately licensed soundtrack releases are not used.

Source: the official [`sintel_trailer-1080p.mp4`](https://download.blender.org/durian/trailer/sintel_trailer-1080p.mp4), **H.264, 1920×1080 at 24 FPS**, AAC 48 kHz stereo, 52.208 seconds. The downloaded MP4 is played directly in Fastiplayer. Excerpts appear inside the recorded application; the screen capture is edited and compressed, with English titles added. The source movie and standalone audio are not bundled in the repository.

## Logo

[`LOGO.png`](../../LOGO.png) is the unchanged original sign. `fastiplayer-mark.svg` embeds those exact PNG bytes on a permanent dark background with padding. It neither redraws nor recolors the sign, keeping the white mark visible in light and dark GitHub themes.

## SHA-256

| Artifact | SHA-256 |
| --- | --- |
| Trailer cover | `038e1ce5b04251e8a27a0293f9ad6420f137b83c86df6ed3fbe09dc32f90848c` |
| Main alias (2026-09-06 playback screenshot) | `52119803082f4b8d6b36e469357ea2702a4359786555e02e3b73e8670f2f2f15` |
| Playback screenshot (2026-10-11) | `415eba4d1e0ca6dfd52693b59f93a2702fad54ea7176885d8a01833713a6eebd` |
| Queue screenshot (2026-10-11) | `e0728be8c1c61ebc42feeebede7b4bd3c25eb3d5ea275340e8274006b8826719` |
| 2026-10-11 capture executable | `a594701a5db4f3a21dc465615b43b3dcaaeb7c09236af7ffc87ebdb29b3f5ac8` |
| Settings screenshot | `6bc96bfc351a93ee45172a0b3b7f0ff2b4387050c5c2d8076e40d2a0f39fbe74` |
| YouTube upload MP4 | `558470d240ec3c6df155bc12694416facecf5e42a7c7e1853170dabb58f7bb94` |
| Official Sintel Trailer MP4 | `34bbd52a4b89fdf63c8ace50b268da26653a59508288100cd3c23de276db7931` |
| Capture executable | `b94e278e49cb5fb725c89eb9cfb0460346cf8ed4a85189219b733714c61a5e91` |
| Logo presentation | `5e30f2766b499d56b4c22e56869bff21af7aebb9c6ac3333db2eb13eecc1d9a6` |
| Original logo | `a97b93fd3cebed1911194f6ec45d3f13ed5d0cef6c4f1d9d014e60c4e1b4c3f8` |
