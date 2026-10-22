# Local hardware-decode CPU parity — AMD, 2026-10-22

**Fastiplayer showed no frame loss on all 17 hardware scenes. It used 5–17× less
GPU render-engine time and 38–58 % of the RSS of the reference player, mpv.
Its CPU use was not proven worse on any scene, but not proven better either.**
After 9 alternating pairs, all 17 CPU comparisons remain INCONCLUSIVE, and none
fits the session's ±0.5 percentage-point exit band. The planned exit criterion
was therefore **not met**. Equal smoothness is not established. No PARITY label
is emitted.

The software-decode qualification of session 08 is in
[Software decode (session 08)](#software-decode-session-08) below.

Task: session 06c of the CPU-parity plan (`user/cpu-parity/`);
tracking goal: [#10](https://github.com/Bogdan7c/fastiplayer/issues/10).
Measurement tools: [local protocol, version 3](tools/local/README.md#measurement-protocol-version-3).

## Provenance and scope

- Production source: `90321b08bd68bad1fe125762eed40d1b1b778a45` (tree clean).
- Fastiplayer release SHA-256:
  `31271b45e94866244116172394f0f345967835d4607d160a27398c4e469cd47c`, built with
  `cargo build --release -p app-egui --locked` (default features, no LTO;
  `Cargo.lock` `d08b01e8…3917`). Rust 1.99.0.
- Reference: mpv 0.41.0 (`/usr/bin/mpv` SHA-256 `390d7670…5076`), launched as
  `--no-config --hwdec=auto --fs --volume=0`. mpv selected `hwdec-current = vulkan`
  (Vulkan Video, zero-copy) with `vo = gpu-next` in every attempt.
- Fastiplayer: `--fullscreen`, Vulkan renderer, VA-API decoding with DMA-BUF import.
  The backend was proven from the runtime log in every attempt.
- Host: AMD Ryzen 7 7840HS / Radeon 780M, Linux 7.2.10-1-cachyos, Mesa 26.2.4,
  libva 2.24.1, PipeWire 1.6.9, KDE Plasma/KWin 6.8.0 Wayland session.
  Both players ran under **XWayland**, fullscreen via the KWin script, at
  1920×1080 and reported 59.96 Hz. systemd 262, libxcb 1.17.0, Python 3.14.8,
  FFmpeg 9.0.2 (matrix generation only).
- Governor/EPP: performance/performance; power profile: performance. AC telemetry
  and a usable CPU thermal zone were unavailable on this host. One-minute load
  average during the series: 0.04–2.06. Collector overhead: 0.30–0.60 % of one
  core, measured separately and not included in either player.
- Observer tools: commit `90321b08`. The per-file SHA-256 values are in the
  [compressed per-run evidence](local-cpu-parity-2026-10-22.json.gz) (`tool_sha256`).
- Fresh XDG config/data/cache/state for each attempt. The media file is fully
  pre-read before spawn. Warmup: at least 12 s plus the boundary RPC. Window: 20 s.
  Samples: 1 Hz.
- Fastiplayer config: the session 06 hardware template (`preferred_backend =
  "hardware"`, volume 0, Vulkan FIFO, ordinary animations). SHA-256 `27176706…a0`.

The series ran from 01:32 to 04:36 local time on an otherwise idle live desktop.
The mouse pointer rested away from the player panels, as protocol v3 requires.
No compilation or profiling overlapped the series.

## Media

One owner-selected 4K60 SDR master, a public nature video that is **not
redistributed**: master SHA-256 `88344504…abad`, a 45-second segment starting
at 300 s. `make_matrix.py` (in the tools directory) builds every variant from it:

- 2-second GOP, 8-bit 4:2:0 (one HEVC Main10 extra);
- AAC 256k in MP4 or Opus 160k in WebM;
- 30 fps variants drop every second frame.

Bitrates sit near the upper end of each codec level ("heavy but real-world").
Each file was verified with ffprobe and its hash was rechecked before and after
each attempt.

| Scene | Codec / profile | Size | fps | Video Mbit/s actual / target | Audio | SHA-256 |
|---|---|---|---:|---|---|---|
| av1-1080p30 | AV1 Main | 1920×1080 | 30 | 9.9 / 10 | AAC | `8835d7910d1f2c1d3b2d63effd0b0389af62a8d9485b6c8cf125030b037f914e` |
| av1-1080p60 | AV1 Main | 1920×1080 | 60 | 14.8 / 15 | AAC | `60f1a925350a6b92784c0bc4d5e107380467db923d9f94b10c454576ceaca06b` |
| av1-4k30 | AV1 Main | 3840×2160 | 30 | 28.1 / 30 | AAC | `89948c6115cb016b20fa2b8c6a11cdce44def85d359d10f89f12f290a3ce5a0d` |
| av1-4k60 | AV1 Main | 3840×2160 | 60 | 39.2 / 40 | AAC | `3fdddb1855b8d72747a44044c7778f1d226d80dbc3c61cf9f9d0b2296210dac3` |
| h264-1080p30 | H.264 High | 1920×1080 | 30 | 24.4 / 25 | AAC | `ee7b8241348a72f9040ea9dd83a4cc4e01cb86a294edf61096793d0accb837fa` |
| h264-1080p60 | H.264 High | 1920×1080 | 60 | 39.2 / 40 | AAC | `f9799510bad2d9588e5f9be4de9aa48a2ce56a9919818d73108a032f2da9cb9d` |
| h264-4k30 | H.264 High | 3840×2160 | 30 | 77.8 / 80 | AAC | `ca645e6ebec1f3ea19db8ab37009e9dee70979dd847a825fcf2a7a5c66c7e039` |
| h264-4k60 | H.264 High | 3840×2160 | 60 | 116.8 / 120 | AAC | `467bc1f6bcb07f5da87bb1ec2fc85998260ce1f42166ff201b4ef254ef11f663` |
| hevc-1080p30 | HEVC Main | 1920×1080 | 30 | 14.5 / 15 | AAC | `b7781b6fa3d3022aec8d9d4b79d432d21a6769f84a61ad2fd83f664e5fb24114` |
| hevc-1080p60 | HEVC Main | 1920×1080 | 60 | 19.4 / 20 | AAC | `08c8f520d17a6dc4a0faec248bbeea70e8f821fca13bafeb287bff3e582f6021` |
| hevc-4k30 | HEVC Main | 3840×2160 | 30 | 38.9 / 40 | AAC | `cc56485c7391fc6f11ad3315b6dc1ddbb1962661eeb6b2dbf43434336b3b31f6` |
| hevc-4k60 | HEVC Main | 3840×2160 | 60 | 58.6 / 60 | AAC | `09986b5a08066a75f569ea527a64827dac36977743db89ed8df816f82db24461` |
| hevc10-4k60 | HEVC Main10 | 3840×2160 | 60 | 58.5 / 60 | AAC | `fcd0def14cb5cc4dd27b5e2546c5b1091810cc331c327a8d24eb5a3fbe6c90b0` |
| vp9-1080p30 | VP9 Profile 0 | 1920×1080 | 30 | 10.4 / 12 | Opus | `6f1bb9a4352287f05bb4f17530dcd8a54aaab56593f269a0634df807390147cc` |
| vp9-1080p60 | VP9 Profile 0 | 1920×1080 | 60 | 18.7 / 20 | Opus | `2df907823a7c304bdba098a12252296e8d6657f47473d4c162f3df4173dc4a3a` |
| vp9-4k30 | VP9 Profile 0 | 3840×2160 | 30 | 29.4 / 35 | Opus | `59166aeefdc4e40bd521dff6ebc59a5f24ac97f931f6faa402b97c99b88cb970` |
| vp9-4k60 | VP9 Profile 0 | 3840×2160 | 60 | 45.6 / 50 | Opus | `616b393bbeed683e913b72e458fe2eb4554fee281f3ecaf53f97fd57afdaf5f9` |

## Attempts

- **Series: 340 of 340 attempts were valid resource observations**, none
  excluded or replaced:
  - quiet mode: 17 scenes × 2 players × 9 repetitions;
  - diagnostic mode: 17 scenes × 2 players × 1 repetition.
- Player order and quiet/diagnostic order alternated (AB/BA) between repetitions.
- Every quiet cohort was INCONCLUSIVE at 3 and at 6 pairs, so each was extended
  to 9 pairs, as the predeclared rule requires. The series stopped there.
- A separate pilot (8 short diagnostic attempts on vp9/hevc × 1080p30/4K60) was
  also valid. Its screenshots, taken outside the CPU window, show Fastiplayer
  fullscreen with its controls auto-hidden. The pilot is not scored.

## Quality gate (diagnostic round)

The gate: Fastiplayer unique `(PTS, render generation, decoded generation)`
handoff identities must reach ≥ 99 % of `source fps × log window`, and the
reference player must lose 0 frames.

**17/17 PASS.** Fastiplayer showed 1199–1201 unique frames per 1200 expected
at 60 fps (including Main10) and 600 of 600 at 30 fps. mpv recorded
`frame-drop-count` + `decoder-frame-drop-count` = 0 and
`vo-delayed-frame-count` = 0 in every attempt.

These are surface handoffs, not physical scanout. The gate does not prove equal
smoothness.

## CPU (quiet mode, 9 pairs per scene)

Metric: CPU of the player's cgroup tree (all threads and children), % of one
logical core, over the 20-second window.

- The interval is `mean ± (10 · sd / √n + 0.10)` percentage points, with the
  counter-resolution floor of 0.10.
- Δ = Fastiplayer − mpv; a negative value means Fastiplayer used less CPU.
- sd columns: the standard deviation of each player's own 9 values.

| Scene | Fastiplayer % | mpv % | Δ mean, pp | Interval, pp | Verdict | Within ±0.5 | sd FP / mpv |
|---|---:|---:|---:|---|---|---|---|
| av1-1080p30 | 4.36 | 4.23 | +0.13 | [−0.38, +0.64] | INCONCLUSIVE | no | 0.11 / 0.07 |
| av1-1080p60 | 4.98 | 6.04 | −1.06 | [−2.40, +0.28] | INCONCLUSIVE | no | 0.25 / 0.18 |
| av1-4k30 | 4.60 | 4.60 | +0.00 | [−0.92, +0.93] | INCONCLUSIVE | no | 0.19 / 0.09 |
| av1-4k60 | 5.02 | 6.06 | −1.04 | [−2.17, +0.10] | INCONCLUSIVE | no | 0.27 / 0.21 |
| h264-1080p30 | 4.30 | 4.35 | −0.04 | [−0.74, +0.66] | INCONCLUSIVE | no | 0.12 / 0.14 |
| h264-1080p60 | 5.19 | 5.73 | −0.53 | [−2.00, +0.93] | INCONCLUSIVE | no | 0.19 / 0.43 |
| h264-4k30 | 5.02 | 5.04 | −0.02 | [−0.59, +0.55] | INCONCLUSIVE | no | 0.09 / 0.14 |
| h264-4k60 | 6.33 | 6.83 | −0.50 | [−2.92, +1.92] | INCONCLUSIVE | no | 0.30 / 0.63 |
| hevc-1080p30 | 4.23 | 4.40 | −0.18 | [−0.89, +0.53] | INCONCLUSIVE | no | 0.18 / 0.08 |
| hevc-1080p60 | 4.94 | 5.94 | −1.00 | [−2.38, +0.39] | INCONCLUSIVE | no | 0.24 / 0.37 |
| hevc-4k30 | 4.62 | 5.08 | −0.46 | [−1.28, +0.36] | INCONCLUSIVE | no | 0.14 / 0.12 |
| hevc-4k60 | 5.62 | 6.83 | −1.22 | [−3.17, +0.74] | INCONCLUSIVE | no | 0.21 / 0.50 |
| hevc10-4k60 | 5.68 | 7.31 | −1.63 | [−4.15, +0.89] | INCONCLUSIVE | no | 0.16 / 0.62 |
| vp9-1080p30 | 4.60 | 4.41 | +0.19 | [−0.40, +0.79] | INCONCLUSIVE | no | 0.08 / 0.10 |
| vp9-1080p60 | 5.02 | 5.40 | −0.38 | [−1.68, +0.91] | INCONCLUSIVE | no | 0.14 / 0.35 |
| vp9-4k30 | 4.84 | 5.11 | −0.26 | [−0.93, +0.40] | INCONCLUSIVE | no | 0.10 / 0.09 |
| vp9-4k60 | 5.43 | 6.30 | −0.86 | [−3.56, +1.84] | INCONCLUSIVE | no | 0.21 / 0.78 |

How to read this:

- **No WORSE, no WIN.** The conservative interval (t = 10, chosen so that three
  sequential looks stay below a 3 % error per scene) needs a paired standard
  deviation of about 0.12 pp to fit inside ±0.5 pp, even when the true difference
  is zero. The observed paired spread was 0.12–0.78 pp.
- **60 fps scenes.** Most of the spread came from the reference: within one scene,
  mpv ranged up to 2.3 pp between runs (for example 5.4–7.7 % on vp9-4k60), while
  Fastiplayer's own range was at most 0.9 pp. The mean difference favours
  Fastiplayer on all nine 60 fps scenes. These are descriptive observations,
  not verdicts.
- **30 fps scenes.** The mean difference is between −0.46 and +0.19 pp. On
  `vp9-1080p30` and `av1-1080p30`, Fastiplayer used more CPU in 8 of 9 pairs
  each, by up to +0.4 pp; the remaining pair was within 0.03 pp. This is a
  likely small disadvantage that the protocol did not classify as WORSE.
- Diagnostic-mode pairs (1 per scene, with trace logging) are descriptive
  observer controls only and are kept in the evidence file.

## GPU engine time (quiet mode, 9 pairs)

Metric: DRM fdinfo `drm-engine-*` busy time of the player's process tree over
the window, as a % of wall time. It is a separate metric and is never added to CPU.

- `gfx` is the render engine and carries a paired verdict (same rule, floor 0).
- `VCN` is the video block, which amdgpu reports as engine `enc` on this GPU even
  while it decodes. It is descriptive only.
- Compositor work is not included.

| Scene | gfx Fastiplayer % | gfx mpv % | gfx Δ interval, pp | gfx verdict | VCN Fastiplayer % | VCN mpv % |
|---|---:|---:|---|---|---:|---:|
| av1-1080p30 | 0.97 | 4.74 | [−4.23, −3.31] | WIN | 8.3 | 8.5 |
| av1-1080p60 | 1.58 | 9.21 | [−7.90, −7.35] | WIN | 15.8 | 16.5 |
| av1-4k30 | 1.42 | 18.73 | [−17.77, −16.84] | WIN | 25.7 | 26.0 |
| av1-4k60 | 2.30 | 37.38 | [−35.50, −34.65] | WIN | 41.0 | 46.6 |
| h264-1080p30 | 0.95 | 4.64 | [−3.98, −3.39] | WIN | 7.7 | 7.9 |
| h264-1080p60 | 1.57 | 8.98 | [−7.90, −6.91] | WIN | 14.2 | 13.9 |
| h264-4k30 | 1.31 | 18.89 | [−17.89, −17.28] | WIN | 23.0 | 23.8 |
| h264-4k60 | 2.22 | 37.35 | [−35.52, −34.74] | WIN | 39.9 | 39.2 |
| hevc-1080p30 | 0.95 | 4.70 | [−4.07, −3.41] | WIN | 8.0 | 8.5 |
| hevc-1080p60 | 1.58 | 9.09 | [−8.06, −6.95] | WIN | 15.2 | 15.5 |
| hevc-4k30 | 1.34 | 18.79 | [−17.77, −17.14] | WIN | 24.7 | 25.2 |
| hevc-4k60 | 2.17 | 37.04 | [−35.21, −34.53] | WIN | 45.0 | 48.4 |
| hevc10-4k60 | 3.35 | 37.38 | [−34.49, −33.57] | WIN | 46.0 | 49.0 |
| vp9-1080p30 | 0.98 | 4.74 | [−4.09, −3.42] | WIN | 9.8 | 10.0 |
| vp9-1080p60 | 1.58 | 8.97 | [−7.59, −7.19] | WIN | 19.0 | 19.1 |
| vp9-4k30 | 1.39 | 18.71 | [−17.48, −17.16] | WIN | 30.9 | 31.0 |
| vp9-4k60 | 2.15 | 37.24 | [−35.69, −34.50] | WIN | 53.8 | 56.3 |

The gfx gap is mostly scaling. mpv's default `gpu-next` output scales 4K to
1080p with a high-quality filter on the render engine. Fastiplayer uses a simpler
sampler. This report does not compare image quality. Both players decode on the
same VCN block with similar load.

## Memory (quiet mode, 9 attempts per player)

Metrics: process-tree RSS and PSS from `smaps_rollup`, in MiB. "Mean" is the
average of the 1 Hz window samples, averaged over attempts. "Max" is the largest
sampled value. Charged cgroup memory and GPU allocations are not included.

| Scene | RSS FP mean / max | RSS mpv mean | PSS FP mean / max | PSS mpv mean |
|---|---:|---:|---:|---:|
| av1-1080p30 | 114 / 117 | 203 | 69 / 72 | 148 |
| av1-1080p60 | 114 / 115 | 204 | 69 / 70 | 150 |
| av1-4k30 | 123 / 126 | 214 | 78 / 81 | 160 |
| av1-4k60 | 124 / 126 | 217 | 78 / 81 | 163 |
| h264-1080p30 | 120 / 121 | 229 | 74 / 76 | 174 |
| h264-1080p60 | 118 / 119 | 231 | 73 / 74 | 176 |
| h264-4k30 | 137 / 139 | 297 | 92 / 94 | 242 |
| h264-4k60 | 135 / 139 | 300 | 90 / 94 | 245 |
| hevc-1080p30 | 114 / 117 | 233 | 69 / 72 | 178 |
| hevc-1080p60 | 114 / 115 | 232 | 68 / 69 | 178 |
| hevc-4k30 | 121 / 124 | 321 | 76 / 79 | 267 |
| hevc-4k60 | 119 / 122 | 324 | 74 / 77 | 270 |
| hevc10-4k60 | 120 / 122 | 329 | 74 / 76 | 274 |
| vp9-1080p30 | 114 / 116 | 199 | 69 / 70 | 146 |
| vp9-1080p60 | 114 / 115 | 202 | 69 / 70 | 148 |
| vp9-4k30 | 121 / 124 | 213 | 76 / 78 | 159 |
| vp9-4k60 | 120 / 123 | 214 | 75 / 77 | 161 |

**Against Fastiplayer's own baseline**, the first quiet round of session 06
(protocol v2, same files, build `dab9e2c7`): mean RSS changed by −2.9 to
+2.3 MiB and mean PSS by −4.4 to +0.8 MiB per scene, within the ±5 MiB noise
band. Session 00 (different files of the same resolution) recorded 117 MiB for
H.264 1080p60 and 118 MiB for HEVC 4K60. No systematic growth was observed.

## Limitations

- The ±0.5 pp exit band was not reached. The INCONCLUSIVE results say "difference
  not resolved", not "equal".
- 45-second clips, one master, one machine, XWayland only. Native Wayland, battery
  power and the ThinkPad T480s (Intel) were not measured here.
- mpv decodes through Vulkan Video and Fastiplayer through VA-API: two APIs on
  the same VCN block. The two players' UIs, renderers and scalers differ. This is
  a user-visible default-behaviour comparison, not a comparison of identical
  pipelines.
- Quality evidence is limited to surface-handoff identities and the reference's
  drop counters. It does not prove physical scanout, equal smoothness or audible
  output.
- Protocol v3 numbers are not comparable with the v1 baseline of 2026-09-08 or with
  the v2 descriptive round of session 06 (different reference, entry into fullscreen
  and tools).
- Raw artifacts (runtime logs, screenshots, full per-sample observations) are kept
  privately because they contain local paths. The compressed evidence file holds
  the sanitized per-attempt metrics, summaries and quality gates.

## Software decode (session 08)

**Same protocol (version 3), same reference player (mpv), software decoding on
both sides.** Fastiplayer showed no frame loss on all 5 scenes. Its GPU
render-engine time was 2–6× lower on every scene. CPU results:

- **WIN** on both AV1 4K60 scenes, by −98 pp on the tiled Big Buck Bunny file;
- **INCONCLUSIVE** on HEVC 4K60 (mean −5.4 pp in Fastiplayer's favour);
- **WORSE by about 3.5 pp** on H.264 1080p60 and VP9 4K60.

mpv lost frames on one scene (single-tile AV1 4K60). That scene's CPU result
holds **only with that caveat** and claims no superiority. The session's exit
criterion (WIN, or INCONCLUSIVE inside ±1 pp, on every scene) was **not met** on 3
of 5 scenes.

Task: session 08 of `user/cpu-parity/`. Before this series, session 08 changed the
software decoder's thread policy:

- AV1 is capped at 4 threads up to 1080p and at 6 above, and decodes 2 frames in
  parallel above 1080p;
- H.264, HEVC and VP9 keep "cores − 2" threads;
- it also fixed a worker busy-spin that occurred while a slow decoder kept its
  packet channel full.

### Provenance and scope

- Production source: `b817df134c6adca9a2c41f659283a91b6e8f5c87` (tree clean for
  production code). Fastiplayer release SHA-256
  `73ff53300b680f900ae65097f9e1299cf51dff02e4037bf8aa46d012279b244f`, rebuilt
  bit-identically from that commit with `cargo build --release -p app-egui --locked`
  (default features: vendored LGPL FFmpeg 8.1.3 + dav1d 1.5.4, no LTO).
- Reference: mpv 0.41.0 (same binary as the hardware series), launched as
  `--no-config --hwdec=no --fs --volume=0`. `--hwdec=no` is mpv's default.
  `hwdec-current = no` was proven at both window boundaries of every attempt.
  mpv's thread choices are its own defaults: libavcodec with `cores + 1` (16) frame
  threads, and dav1d with its automatic thread count and frame delay.
- Fastiplayer: `--fullscreen`, the hardware series' configuration with
  `preferred_backend = "software"`, and `sw_decode_threads = 0` (automatic policy).
  The `ffmpeg-host-upload-wgpu` plan, with no VA-API trace, was proven from the
  runtime log of every attempt.
- Host, session, display, timing (12 s warm-up, 20 s window), XWayland
  fullscreen and alternation are as in the hardware series above.
- New plan fields:
  - `decode = software`;
  - `exit_band_percentage_points = 1.0` (the session-08 criterion is ±1 pp);
  - `reference_losses = annotate`: reference losses mark the scene
    `REFERENCE_LOSSES` instead of stopping the series. Fastiplayer losses still
    fail.

  See [the tools README](tools/local/README.md#measurement-protocol-version-3).

### Media

| Scene | File | SHA-256 (prefix) |
|---|---|---|
| h264-1080p60 | matrix `1080p60_h264-high_40mbps_aac.mp4` | `f9799510bad2d958` |
| hevc-4k60 | matrix `4k60_hevc-main_60mbps_aac.mp4` | `09986b5a08066a75` |
| vp9-4k60 | matrix `4k60_vp9-profile0_50mbps_opus.webm` | `616b393bbeed683e` |
| av1-4k60 | matrix `4k60_av1-main_40mbps_aac.mp4` (SVT-AV1, a single tile per frame) | `3fdddb1855b8d727` |
| av1bbb-4k60 | Big Buck Bunny 4K60 (Blender Foundation, CC BY 3.0), AV1 8-bit with tiles, AAC 5.1, 9.4 Mbit/s, 4–5 s GOP | `e647620fa682a1ca` |

Matrix files are the 45-second clips described in [Media](#media) above.

### Attempts

- 58 of 58 attempts were valid, with none excluded or replaced.
- Every scene had 1 diagnostic pair.
- Quiet pairs per scene:
  - 3 for av1-4k60, av1bbb-4k60 and vp9-4k60 (verdict reached at the first look);
  - 6 for h264-1080p60;
  - 9 for hevc-4k60.
- A pilot (one diagnostic attempt per player on av1-4k60) was valid and is not
  scored.

### Quality gate

| Scene | Fastiplayer unique frames / expected | mpv lost frames | Gate |
|---|---|---|---|
| h264-1080p60 | 1201 / 1200.3 | 0 in all 7 attempts | PASS |
| hevc-4k60 | 1201 / 1200.4 | 0 in all 10 | PASS |
| vp9-4k60 | 1201 / 1200.3 | 0 in all 4 | PASS |
| av1-4k60 | 1201 / 1200.3 | **3 in every one of 4** | REFERENCE_LOSSES |
| av1bbb-4k60 | 1200 / 1200.2 | 0 in all 4 | PASS |

### CPU, GPU and memory (quiet mode)

Process-tree CPU in % of one logical CPU. The difference is Fastiplayer − mpv,
with a conservative Student interval (t = 10).

| Scene | Pairs | Fastiplayer CPU, mean (range) | mpv CPU, mean (range) | Difference | Interval | CPU verdict | GPU gfx busy %, Fastiplayer / mpv (verdict) | RSS MiB, Fastiplayer / mpv | PSS MiB, Fastiplayer / mpv |
|---|---|---|---|---|---|---|---|---|---|
| h264-1080p60 | 6 | 52.4 (51.7–53.0) | 48.8 (48.4–49.0) | +3.66 | [+1.11; +6.21] | **WORSE** | 3.82 / 8.32 (WIN) | 241 / 274 | 231 / 260 |
| hevc-4k60 | 9 | 173.3 (170.9–176.3) | 178.7 (177.9–180.2) | −5.38 | [−12.59; +1.83] | INCONCLUSIVE | 6.62 / 37.13 (WIN) | **672 / 515** | **662 / 500** |
| vp9-4k60 | 3 | 113.6 (112.9–114.5) | 110.2 (110.0–110.5) | +3.35 | [+0.20; +6.50] | **WORSE** | 6.59 / 36.89 (WIN) | **421 / 236** | **411 / 222** |
| av1-4k60 ¹ | 3 | 126.8 (126.1–127.7) | 132.0 (131.8–132.1) | −5.23 | [−9.62; −0.84] | WIN ¹ | 6.63 / 37.00 (WIN) | 404 / 572 | 395 / 558 |
| av1bbb-4k60 | 3 | 113.7 (112.8–114.3) | 211.5 (207.3–213.6) | −97.78 | [−113.99; −81.58] | **WIN** | 6.81 / 38.88 (WIN) | 404 / 579 | 395 / 565 |

¹ **With reference frame losses:** mpv dropped 3 frames in every attempt on this
scene, and Fastiplayer dropped none. The CPU difference is reported but is not a
claim of superiority.

**Where the gap comes from (diagnostic per-thread split, outside the protocol):**

- On H.264 1080p60 and VP9 4K60 the decoders cost the same or less in
  Fastiplayer: 43.6 vs 44.9 % and ~98 vs 105 %. Both players use libavcodec with
  frame threading.
- The difference is Fastiplayer's presentation path:
  - main thread ≈ 6 % on 1080p60 and ≈ 10 % on 4K60;
  - mpv's `vo` thread 2.4 % and 3.3 %.
- Fastiplayer copies each decoded frame into a staging buffer on its main thread
  before the GPU upload (session 07).
- On AV1 the thread cap removes dav1d's synchronisation overhead. mpv runs dav1d
  with all 16 logical CPUs.

### Software-decode limitations

- **Exit criterion not met** on h264-1080p60 and vp9-4k60 (WORSE) and on hevc-4k60
  (interval wider than ±1 pp).
- **Memory:** Fastiplayer's RSS is higher than mpv's on VP9 and HEVC 4K60, and
  lower on H.264 and AV1. AV1 4K grew by about +110 MiB in session 08, the
  owner-accepted price of decoding 2 frames in parallel.
- Session 08b (planned) covers the presentation-path cost and the 4K memory gap.
- One machine, XWayland only, 45-second matrix clips plus one long file. The weak
  Intel laptop (T480s) was not measured, so the thread caps there are untested.
- The players' renderers, scalers, demuxers and FFmpeg builds differ. mpv uses
  the distribution FFmpeg, Fastiplayer the vendored FFmpeg 8.1.3. This is a
  default-behaviour comparison.
- Sanitized per-attempt evidence: `local-cpu-parity-sw-2026-10-22.json.gz`.
