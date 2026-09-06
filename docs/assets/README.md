# Runtime assets and attribution

The [72-second trailer](https://www.youtube.com/watch?v=eMfzBhpSF8M) and these genuine Fastiplayer captures were made on **2026-09-06** on the current development computer: **AMD Ryzen 7 7840HS / Radeon 780M**, Linux **7.2.2-1-cachyos**, KDE Plasma Wayland. The player used **VA-API H.264 hardware decoding**, NV12 DMA-BUF video frames, and **WGPU/Vulkan rendering** through XWayland.

Source revision: [`0638b476bba429e4f80d617c3a5e73319cd80984`](https://github.com/Bogdan7c/fastiplayer/commit/0638b476bba429e4f80d617c3a5e73319cd80984).
Build: `cargo build -p app-egui --release --locked`, default features, Rust **1.96.0**. Package versions remain **0.1.0-alpha.1**. The capture uses a separate temporary application profile, preserving the user's configuration.

## Published assets

- `fastiplayer-trailer-cover.png`: a genuine final-card frame at 01:10.5 from the trailer, including native Resolve titles over the captured application.
- `fastiplayer-playback.png`: resumed playback following a live timeline drag.
- `fastiplayer-main.png`: identical bytes to the playback screenshot, preserving the compatible path used in existing alpha release notes.
- `fastiplayer-queue.png`: the queue sidebar with the authorized Sintel Trailer alongside playback.
- `fastiplayer-settings.png`: video color settings during playback, with saturation deliberately raised to 2.45 to show its effect.

Screenshots are **1920×1080 PNG** frames extracted from the real OBS recordings, with the recorded cursor included. The cover is extracted from the finished trailer. The player UI and movie frames are not retouched. The [demo page](../demo.md) supplies timecodes and distinguishes the demonstrated interactions from future features.

Video is hosted on YouTube. Large recordings, film media, the DNxHR master, and the Resolve `.drp` project are kept outside Git. The previous silent 39-second MP4 is superseded by this trailer; its historical bytes remain in Git history. Existing releases and tags are unchanged.

These captures are **not performance measurements** and are **not the historical ThinkPad T480s captures**. The [T480s report](../benchmarks/thinkpad-t480s.md) and [tagged asset directory](https://github.com/Bogdan7c/fastiplayer/tree/v0.1.0-alpha.1/docs/assets) retain their own provenance. The September 5 presentation and Big Buck Bunny attribution remain in [the preceding asset documentation](https://github.com/Bogdan7c/fastiplayer/blob/0638b476bba429e4f80d617c3a5e73319cd80984/docs/assets/README.md).

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
| Playback screenshot / main alias | `52119803082f4b8d6b36e469357ea2702a4359786555e02e3b73e8670f2f2f15` |
| Queue screenshot | `b0177f1bf5548232a469b1020846ca431a983850bc013f47cb0367d6c6970d1c` |
| Settings screenshot | `6bc96bfc351a93ee45172a0b3b7f0ff2b4387050c5c2d8076e40d2a0f39fbe74` |
| YouTube upload MP4 | `558470d240ec3c6df155bc12694416facecf5e42a7c7e1853170dabb58f7bb94` |
| Official Sintel Trailer MP4 | `34bbd52a4b89fdf63c8ace50b268da26653a59508288100cd3c23de276db7931` |
| Capture executable | `b94e278e49cb5fb725c89eb9cfb0460346cf8ed4a85189219b733714c61a5e91` |
| Logo presentation | `5e30f2766b499d56b4c22e56869bff21af7aebb9c6ac3333db2eb13eecc1d9a6` |
| Original logo | `a97b93fd3cebed1911194f6ec45d3f13ed5d0cef6c4f1d9d014e60c4e1b4c3f8` |
