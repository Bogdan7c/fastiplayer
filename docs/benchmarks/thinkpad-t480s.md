# ThinkPad T480s playback evidence

Product names and result labels were normalized to Fastiplayer after the rename. All measurements, dates, samples and original source/binary hashes still refer to the historical revisions recorded below. Original labels remain available in Git history; these results are not measurements of the renamed build.

**Measured on 2026-09-05.** All 18 final warm-ups and 30 scored attempts completed on AC power: three warm-ups and five 60-second measurements for each player/scenario. No final scored attempt was excluded. Hardware H.264/HEVC and software AV1 remain separate workloads.

The [machine-readable evidence](thinkpad-t480s.json) contains every final raw sample, render identity, VLC counter query, validation result and cohort statistic. The [preparation archive](thinkpad-t480s-preparation.json) retains 100 earlier attempts with their original provenance, including failed and excluded attempts. The native Wayland screenshot validation is also retained separately in the final JSON. None of those preparation/capture measurements enters the scored statistics.

## Results

### Hardware playback: process CPU and resident memory

These are whole-player resource observations for the same synthetic file, hardware decode mode, display/session, fullscreen window, mute policy and collector. Fastiplayer uses Vulkan/DMA-BUF with per-frame diagnostic tracing; VLC uses OpenGL/VA-API conversion with verbose diagnostics. Their rendering implementations and diagnostic overhead differ. Exact equality of unique physically displayed frames is not established, so this table is not a decoder-efficiency, smoothness or speed ranking.

Each cell is **p50 / p95 / observed min–max** across five runs. CPU uses **100% = one logical CPU**. RSS is the per-run mean of resident-memory samples, converted from KiB to MiB; it does not measure total system or GPU memory. With five runs, nearest-rank p95 equals the maximum.

| Synthetic source | Player | CPU (%) — p50 / p95 / min–max | RSS (MiB) — p50 / p95 / min–max |
| --- | --- | --- | --- |
| H.264 1080p60, hardware | Fastiplayer | 16.88 / 17.15 / 16.73–17.15 | 87.21 / 90.36 / 86.80–90.36 |
| H.264 1080p60, hardware | VLC | 4.77 / 5.02 / 4.72–5.02 | 126.93 / 127.15 / 126.73–127.15 |
| HEVC 4K60, hardware | Fastiplayer | 30.42 / 30.58 / 30.22–30.58 | 87.90 / 90.37 / 87.62–90.37 |
| HEVC 4K60, hardware | VLC | 5.75 / 5.82 / 5.53–5.82 | 300.23 / 300.36 / 299.91–300.36 |

VLC used less process CPU in these hardware scenarios; Fastiplayer used less process RSS. Both continued video and audio processing. Fastiplayer recorded 3528–3596 distinct frame identities for H.264 and 3492–3597 for HEVC across its separate approximately 60-second log intervals. These are not proof that every source frame reached physical scanout and cannot be converted into VLC-compatible dropped frames. VLC's separate counter intervals were approximately 65 seconds and must not be directly compared with those handoff counts. The HEVC source is a simple upscaled synthetic pattern, not a representative natural 4K movie.

### AV1 4K60 SDR: software baseline and control limitations

The owner-supplied Big Buck Bunny AV1 file was tested with software decoding in both players. This T480s exposes no AV1 hardware decode support. Fastiplayer's five-run baseline is:

| Metric | p50 | p95, nearest rank | Observed min–max |
| --- | --- | --- | --- |
| Process CPU, 100% = one logical CPU | 356.70% | 358.16% | 352.21–358.16% |
| Per-run mean RSS | 269.32 MiB | 269.67 MiB | 268.19–269.67 MiB |

Fastiplayer's five observation intervals contained **3549, 3502, 3397, 3573 and 3475 distinct frame identities** handed to the surface. Their PTS spans were 59.983–60.017 seconds. Software decode, demux, audio decoder/output/resume and surface submission were confirmed. This demonstrates continued playback through the render boundary; it is not a claim of perfect 60 FPS or zero dropped frames.

Separately, VLC's own `frames_lost` deltas were **936, 938, 953, 940 and 942**, while `frames_displayed` deltas were **2979, 2961, 2962, 2961 and 2973**. These counters cover separately recorded intervals of approximately 65.005–65.016 seconds. Media time advanced by 65 seconds, audio buffers advanced and the audio-buffer lost delta was zero. The actual selected AV1 decoder was dav1d. These are VLC-specific diagnostics, not the same metric as Fastiplayer frame identities.

**No AV1 CPU/RSS comparison table or speed ratio is published.** Equivalent delivered video work cannot be established with these different frame counters, and the observed VLC losses make an equal-output efficiency claim inappropriate. All VLC raw CPU/RSS samples and descriptive aggregates remain in the JSON. An attempt's `eligible_for_scored_statistics` flag validates that individual measurement; it does not certify cross-player comparability. The evidence supports continued Fastiplayer software playback and VLC's reported losses in this configuration, not a universal performance advantage.

## Source and system

The requested S07 revision was `a1a472bd9dbe7cbfcfea8e1693796552c0b0aeeb`. It was checked out and built, but its CI was not green: coverage run 33922998737 failed first on invalid coverage counters and then on a real HTTP/Ogg Range-request race. Subsequent runtime qualification exposed a media-install snapshot race and an XWayland fullscreen resize race. With the owner's authorization, these causes were fixed before benchmark collection. The original failures remain preparation history, not scored results.

The [preparation archive](thinkpad-t480s-preparation.json) retains the original failed/excluded attempts and later qualification runs with their separate provenance. None is eligible for the scored statistics.

The owner subsequently approved moving the expensive full coverage measurement to a manual workflow while retaining all automatic functional/quality checks and a fast baseline-policy guard. The last full coverage run, 33931378033 at `1cc181e1ea0fd502b6636fba8a9d2ee1367d0687`, passed all three test executions but failed its stable-coordinate ratchet. Its exact report is preserved in the preparation archive. A green automatic CI after this policy change is not a claim of a new successful coverage measurement.

The measurement source is `9165200c772e33785a2d47c3ca886cc6145054a1`, including the runtime fixes and current-frame identity tracing. Release executable SHA-256:

```text
ca4a3d1dc22c58ec513171e5edfc6d9c271f33ff9cc35aeb4cdf8b77a8249372
```

Executed build: `cargo build --release -p app-egui`, then verified from the committed source with `cargo build --release -p app-egui --locked`; the executable checksum remained identical. Both use the repository lockfile and default features. The working tree's remaining changes are evidence tooling and documentation.

The measured source passed automatic CI run 33934211412 and Toolchain policy run 33934211484. Before this final source qualification, the runtime fixes passed 4470 workspace/all-features tests with 23 ignored, strict workspace Clippy, and 300 repetitions of the unchanged HTTP/Ogg playback regression without a failure. The benchmark collector's functional tests measure a real busy process with allocated memory and retain an early process exit as excluded. A second independent review recalculated all 30 CPU/RSS results from samples, checked aggregate statistics, reviewed the screenshot and sanitized artifacts, and assessed the claim boundaries above.

Source and executable identity are cohort-level attestations. Fastiplayer's binary checksum was checked before the final warm-ups and again after collection; the collector did not hash every launch independently. VLC's executable checksum was taken at report assembly and is recorded in JSON. No rebuild, test suite or local compilation ran during scored windows. The desktop and collection tools remained active; this was not an otherwise isolated machine.

| Component | Observed value |
| --- | --- |
| Machine | ThinkPad T480s, verified from DMI model only |
| CPU | Intel Core i5-8350U @ 1.70 GHz; 8 logical CPUs |
| Integrated GPU | Intel UHD Graphics 620, KBL GT2 |
| RAM available to Linux | MemTotal 24,341,448 KiB; this is not an installed-DIMM inventory |
| OS / kernel | CachyOS Linux rolling / 7.2.2-1-cachyos |
| Mesa | 26.2.2-arch3.2; package 26.2.2-2 |
| libva / VA-API | package 2.24.1-1.1; runtime reports 2.24.0; API 1.24 |
| VA-API driver | Intel iHD 26.2.4 |
| Vulkan | device API 1.4.354; loader package 1.4.357.0-1.1 |
| Rust | rustc 1.96.0 (ac68faa20 2026-05-25) |
| VLC | 3.0.23 Vetinari; package 3.0.23_2-13.1 |
| FFmpeg / dav1d | 9.0.1-4.1 / 1.5.4-1.1 |
| Desktop | KDE KWin 6.7.4, Wayland; 1920×1080 at 60.01 Hz; desktop scale 1.25 |
| Power policy | powersave governor, performance EPP, turbo enabled; scored runs require AC |
| Audio | decoder/output active; player volume/gain zero; no audible-output claim |

`/dev/dri/card1` and `/dev/dri/renderD128` were accessible to the desktop runtime. VA-API capabilities include H.264, HEVC Main/Main10 and VP9 profiles 0/2; no AV1 hardware profile is exposed. Actual H.264 and HEVC playback logs confirm stream-specific VA-API configuration, followed by current-frame surface handoffs and audio startup. Fastiplayer uses its VA-API → DMA-BUF → Vulkan/WGPU path. The mandatory AV1 case uses the FFmpeg software → host upload → WGPU path and remains explicitly labelled software.

## Fixtures and rights

| Fixture | Video | Audio | Duration | Origin |
| --- | --- | --- | --- | --- |
| `synthetic-h264-1080p60.mp4` | H.264 High, 1920×1080, 60/1, 8-bit BT.709 SDR | AAC LC, stereo, 48 kHz | 85 s | locally generated moving test pattern and sine |
| `synthetic-hevc-4k60.mp4` | HEVC Main, 3840×2160, 60/1, 8-bit BT.709 SDR | AAC LC, stereo, 48 kHz | 85 s | locally generated 960×540 pattern upscaled to 3840×2160 before encoding |
| `big-buck-bunny-av1-4k60.mp4` | AV1 Main, 3840×2160, 60/1, 8-bit BT.709 SDR | AAC LC, 6 channels, 48 kHz | 634.624 s | owner-supplied Big Buck Bunny AV1 transcode |

The HEVC fixture exercises real 4K60 decoding and presentation of a simple pattern; its spatial complexity does not represent natural 4K content. The two synthetic files are generated from FFmpeg sources and contain no third-party movie footage. Their generation commands are in [make_fixtures.py](tools/t480s/make_fixtures.py). Encoder/driver versions can change the generated bytes; verify checksums before claiming reproduction of this exact corpus.

Big Buck Bunny: © 2008 Blender Foundation / www.bigbuckbunny.org, [Creative Commons Attribution 3.0](https://peach.blender.org/about/). The owner supplied the measured AV1 file. The movie can be legally obtained through the [official download page](https://peach.blender.org/download/) and transcoded, but a byte-identical reacquisition recipe for this particular AV1 encode was not established. A different encode is a new workload, not a reproduction of these results. The media files are not committed to this repository.

```text
b8463a2c5a461b4569538d1049a285b28e6ce2dc8c6a1233ff9776972b7a8d57  synthetic-h264-1080p60.mp4
6b15a9fc8623f5e3bb5f78be4fbbe5635b7a6261042de9cd3353b8b50036d62f  synthetic-hevc-4k60.mp4
e647620fa682a1ca46dcc0c02465f97513241e13e998afd68cdf39c842f00c3b  big-buck-bunny-av1-4k60.mp4
```

## Actual runtime capture

The historical T480s screenshot is preserved in the [tagged asset directory](https://github.com/Bogdan7c/fastiplayer/tree/v0.1.0-alpha.1/docs/assets). It is no longer the current product demo.

This unedited 1280×720 Spectacle capture shows the native Wayland main window at approximately 00:24. It uses the source and binary above. It is separate from the matched fullscreen XWayland measurement configuration. The source is 4K60; the laptop panel and captured window are not 4K displays. Movie attribution: © 2008 Blender Foundation, CC BY 3.0.

Screenshot SHA-256: `3ea5f8781b0b46d9b8ec42baee7db992874123bf5f0a60c60f485c480e950f88`.
## Method and interpretation

The following method was used for the completed scored cohort.

The scored configuration is an **instrumented release build**; instrumentation here means runtime diagnostic tracing in the ordinary Cargo release profile. Fastiplayer logs a current-frame identity after a successful video-containing `Presented` renderer outcome. Formatting and writing those events are included in its CPU cost; their timing overhead is not assumed to be zero. VLC uses its distribution build, verbose runtime diagnostics and the oldrc statistics interface. These are whole-player observations, not an isolated decoder microbenchmark.

Each player/scenario receives at least three warm-up launches followed by five independent 60-second measurements. Every launch receives fresh application configuration, data and cache directories. The entire fixture is sequentially read before spawn to request a warm filesystem page cache; caches are not dropped, and residency is not asserted. Fastiplayer sibling discovery and next-item preloading are disabled. Playback starts at the beginning, unpaused, with volume/gain zero; decoding and the audio output path remain active. No subtitles or HDR processing are enabled in these SDR scenarios.

The CPU window is scheduled at process launch +20 seconds and ends 60 seconds after its first process sample. Preparation, including the before screenshot, must finish before the deadline or the attempt is excluded. This is a launch-relative interval, not exact synchronization to the same media timestamp. Samples retain their actual monotonic timestamps. The before/after screenshot and VLC counter queries have separately recorded times and are not treated as the CPU-window endpoints.

CPU is `100 × Δ(utime + stime) / CLK_TCK / Δmonotonic_seconds`, from Linux `/proc/PID/stat`. **100% means one logical CPU**, and all threads of the launched process are included. RSS is `/proc/PID/smaps_rollup` Rss, sampled approximately once per second. Each run reports sample mean, sample minimum and sample maximum; the maximum is not a lifetime peak. Shared resident mappings are included in RSS. The collector observes child IDs through every thread's `children` file; observing any child excludes the attempt because descendants are outside this collector's accounting. A short-lived child between samples can escape detection. Desktop compositor, audio server and kernel GPU work outside the process are outside the reported scope.

The first/last process samples define CPU time. Samples are deadline-aligned, including the final sample; delayed reads do not create a burst of catch-up samples. An early process exit, missing initial surface/audio proof, absent current-frame events, preparation overrun, capture failure, observed child process or forced kill is retained and excluded. SIGTERM after the completed observation window is normal cleanup, not a playback crash. Slow valid attempts are retained.

Fastiplayer's raw render events contain `(render_generation, decoded_generation, pts_ns)`. Repeated identities are distinguishable from new frames. The log byte offsets and monotonic observation times define a separate interval close to the CPU window; an offset can intersect a log line. This is **surface handoff evidence, not physical scanout**, and its event count is not labelled precise display FPS. UI redraw FPS is not used as unique-frame throughput. There is no validated cross-player dropped-frame counter.

VLC's oldrc `frames displayed` and `frames lost` counters are retained with both query intervals. In VLC 3.0.23, the displayed counter increments after the video-output display call, and the scheduling path can redisplay an existing picture. Lost counters therefore remain VLC-specific diagnostics; neither displayed nor lost values are equated with Fastiplayer's unique PTS handoffs. See the [VLC 3.0.23 video-output implementation](https://github.com/videolan/vlc/blob/3.0.23/src/video_output/video_output.c).

For a homogeneous five-run cohort, p50 is the median, p95 is nearest rank `ceil(0.95 × 5)` (the maximum with this small sample), and range is observed minimum–maximum. CPU statistics aggregate per-run CPU occupancy; RSS statistics aggregate per-run sample means. No codecs, players, revisions, warm-ups or failed attempts are pooled. Five runs do not establish tail reliability, a confidence interval or a general performance ranking.

Startup, first-frame and seek latency distributions are **not measured**. Existing startup acceptance markers prove lifecycle events but do not establish a qualified 30-run latency harness for this session. HDR, subtitles, battery life, energy use and physical display latency are outside this experiment.

## Reproduction commands

Run from the repository root in the same active KDE Wayland desktop session. Required tools include the release player, VLC, FFmpeg with VA-API encoders, Python 3, Spectacle and `qdbus6`. The collector uses Linux `/proc` and `/sys`; the fullscreen helper targets only the process it launched and unloads its temporary KWin script afterward. The current collector requires readable values from every enumerated thermal zone. S09 validation on another computer encountered Linux `ENODATA` while reading a thermal sensor, before spawning the measured process; its two collector tests therefore failed on that host. This is a known collector portability limitation, not a new playback result or a revision of the successful T480s cohort.

```sh
cargo build -p app-egui --release --locked
python3 docs/benchmarks/tools/t480s/test_collect.py
python3 docs/benchmarks/tools/t480s/make_fixtures.py ./benchmark-fixtures
# Place the legally obtained, checksum-verified AV1 file in benchmark-fixtures
# as big-buck-bunny-av1-4k60.mp4 before running the cohorts.
sha256sum benchmark-fixtures/*.mp4
python3 docs/benchmarks/tools/t480s/run_cohorts.py \
  --phase warmup --directory ./benchmark-output \
  --fixtures ./benchmark-fixtures --binary ./target/release/fastiplayer
python3 docs/benchmarks/tools/t480s/run_cohorts.py \
  --phase measurement --directory ./benchmark-output \
  --fixtures ./benchmark-fixtures --binary ./target/release/fastiplayer
# Recalculate the published statistics from validated raw attempts:
python3 docs/benchmarks/tools/t480s/aggregate.py \
  docs/benchmarks/thinkpad-t480s.json
```

Both scored players use fullscreen XWayland on the same 1920×1080 display inside the Wayland session. Fastiplayer renders through Vulkan; VLC uses its OpenGL video output. The helper removes `WAYLAND_DISPLAY`/`WAYLAND_SOCKET` for both launched players. VLC's native Wayland video-output configuration failed during preparation, so it was not mixed into the matched fullscreen cohort. An implementation difference between Vulkan and OpenGL is retained as a whole-player configuration difference, not described as an identical rendering pipeline.

The runner alternates player order within each scenario: Fastiplayer then VLC on odd repetitions, VLC then Fastiplayer on even repetitions. Scenario order is H.264, HEVC, AV1. It reads the full file before every launch, applies the checked-in hardware/software TOML template, and stops on a collection failure. Existing attempt directories are never overwritten. Preserve a failed attempt and use a new ID/directory after diagnosing it. Raw runtime logs and intermediate screenshots remain local because logs can contain local media paths; publish only sanitized observations and reviewed captures.

The runner expands VLC to `--ignore-config --no-one-instance --intf dummy --extraintf oldrc --rc-fake-tty`, a per-attempt Unix control socket, `--vout=gl --fullscreen --no-video-title-show --gain=0 -vv`, and the same fixture. Hardware cases use `--avcodec-hw=vaapi`; AV1 uses `--avcodec-hw=none`. Fastiplayer uses `RUST_LOG=info,fastiplayer::video_render_acceptance=trace` and a fresh per-attempt XDG configuration containing the appropriate checked-in template.

VLC logs identify AV1 decoding as dav1d 1.5.4 with eight threads. Fastiplayer's software configuration uses its automatic thread policy (`sw_decode_threads = 0`); the runtime evidence identifies the FFmpeg software backend, not an independently verified identical inner decoder/thread configuration. These settings describe each whole player and must not be presented as a controlled comparison of the same decoder implementation.
