# Local CPU / memory / playback evidence

These tools run an independent local series. They do not replace or rewrite the
published [T480s tools](../t480s/collect.py). Production playback code is unchanged.
Raw output contains local paths and runtime logs: keep the series outside tracked
files and publish only reviewed, anonymized evidence.

## Ownership and requirements

- `service.py` owns one transient **user** systemd service per attempt. The player
  and all descendants enter its cgroup before exec. No CPU/memory/power limits or
  personal settings are changed. Cleanup targets only that unit and records exit
  status before removing it. Only the ephemeral VLC socket uses a short private
  subdirectory of XDG_RUNTIME_DIR; long artifact paths remain supported.
- `host.py` reads process `/proc/PID/stat`, `smaps_rollup`, cgroup v2 `cpu.stat`,
  separate charged cgroup memory, and background/power observations.
- `evidence.py` checks the exact media inode/current input, actual backend, window,
  advancing media time and audio-consumer evidence. It reuses the existing T480s
  render-event and VLC-counter readers. Fastiplayer MPRIS intentionally omits URLs.
- `window_owner.c` queries XRes local client PID and window geometry, including VLC
  windows that do not expose `_NET_WM_PID`.
- `make_matrix.py` generates the protocol-v2 scene matrix from one master and
  verifies every file; `host.summarize_gpu` turns DRM fdinfo engine time into the
  separate GPU metric.
- `collect.py` owns attempt validation and persistence; `run_series.py` owns
  alternating repetitions and immutable plan/tool hashes; `analyze.py` owns
  per-scenario resource comparisons. `compare_snapshots.py` compares actual before/current
  Fastiplayer series with matching protocol and repetition sets. An invalid attempt is retained and stops the
  runner; it is never silently discarded or replaced.

This implementation targets Linux cgroup v2, user systemd, KDE/KWin **XWayland**,
PipeWire, Python 3.11+, `ffprobe`, VLC 3, `qdbus6`, `pw-dump`, `xprop`, `xrandr`,
and the project's existing fullscreen helper. The small C helper requires a C
compiler and libxcb/libxcb-res headers/libraries. No new Python packages are needed.
Missing capabilities fail explicitly; do not infer hardware absence inside a sandbox.
Run tests/build/playback outside the sandbox per the project policy.

```sh
cc -Wall -Wextra -Werror -O2 docs/benchmarks/tools/local/window_owner.c \
  -lxcb -lxcb-res -o /absolute/artifact-directory/window-owner
python3 -m unittest discover -s docs/benchmarks/tools/local -p 'test_*.py' -v
python3 docs/benchmarks/tools/local/smoke.py /absolute/fastiplayer-spec.json /absolute/new-smoke \
  --reference-spec /absolute/mpv-spec.json
python3 docs/benchmarks/tools/local/collect.py /absolute/spec.json /absolute/new-attempt
python3 docs/benchmarks/tools/local/run_series.py /absolute/plan.json /absolute/new-series
python3 docs/benchmarks/tools/local/analyze.py /absolute/new-series
python3 docs/benchmarks/tools/local/compare_snapshots.py /absolute/original-series \
  /absolute/current-series /absolute/comparison.json
bash scripts/pre-pr-checks.sh
```

The output directory of a single attempt must not exist. A series can resume with
the same plan and tools; an interrupted/invalid attempt requires review. Fixing the
collector requires a **new series**, preserving the old evidence.

## Input contract

A single JSON spec contains `scenario`, `codec` (`h264`, `hevc`, `av1`), `player`
(`fastiplayer`, `vlc`), `mode` (`quiet`, `diagnostic`), `repetition`, positive
`duration_seconds`, `interval_seconds`, and `warmup_seconds` (at least 3).
It also contains absolute `media`, `binary`, `config`, `window_helper` paths and
matching `<name>_sha256` values. Both video and audio are required. The media must
outlast warmup + measurement + two seconds; full content hashes are checked before
launch and again after observation. Isolated XDG config/data/cache/state directories
are created per attempt. The supplied Fastiplayer config is copied there.

A series plan has `common` (shared spec fields), `players` (objects named
`fastiplayer` and `vlc` holding each binary and hash), and `scenarios` (a list of
scenario/media/codec objects, optionally overriding duration). Freeze the plan,
config and hashes **before** the first scored attempt. Use scenario names without
private filenames; paths remain only in private input/raw artifacts.

## Measurement protocol, version 1

The following rules apply before looking at scored results:

1. CPU 100% means one logical core. `process_cpu_percent` is
   `100 * delta(utime + stime) / CLK_TCK / actual_monotonic_interval`, including
   exited threads. It does not include child processes. `tree_cpu_percent` uses
   the independent microsecond cgroup counter over its own timestamped interval,
   including short-lived children. Resource comparisons use **tree CPU**.
   Never sum the two metrics. Record read brackets and PID start-time identity.
2. Record process RSS/PSS at each sample, the steady-window mean and **sampled**
   maximum. RSS sums include shared mappings; PSS apportions shared mappings.
   Vanished-child memory between samples is unknown. Charged cgroup memory/current/
   peak/stat includes different cache/kernel accounting and is never added to
   process memory. Per-allocation GPU memory is N/A until deduplication is qualified;
   cgroup memory is not a claim to measure all GPU/compositor memory. Available
   DRM fdinfo memory counters are retained per device/client, deduplicating FD
   aliases. They are deliberately not summed across clients sharing allocations.
   **GPU memory columns (session 08b, owner decision 22.10.2026).** RSS alone hides
   frames mpv decodes straight into GPU-mapped memory (direct rendering, amdgpu GTT
   mapped from `/dev/dri/renderD*`): those pages have `Rss = 0` in smaps. Each sample
   therefore also sums `drm-memory-gtt`/`drm-memory-vram` over the tree's unique DRM
   clients (`host.gpu_memory_kib`), stored as `gpu_gtt_kib` / `gpu_vram_kib`
   (`mean`/`sampled_max`/`minimum`, KiB). `summary.json` players gain `gtt_mean_mib`,
   `vram_mean_mib`, `*_sampled_max_mib` and `rss_plus_gtt_mean_mib` next to the RSS
   fields; the sum is the honest cross-player memory figure. It is an estimate (clients
   may share allocations, fdinfo cannot tell) and not a whole-GPU total. Older attempts
   without these keys give `null`. The CPU verdict is unaffected. `protocol_version`
   is not bumped: the change only adds fields.
   **Driver naming (session 10, ThinkPad T480s, 2026-10-23).** DRM fdinfo names are
   driver-defined. amdgpu reports the render engine as `gfx` and memory as
   `drm-memory-gtt`/`vram`; i915 reports `render` and `drm-resident-system0`/`local0`.
   `host.RENDER_ENGINE_NAMES` and `host.GPU_MEMORY_FIELDS` map both (amdgpu names keep
   priority, so 780M series compute exactly as before); report columns keep the names
   `gfx`/`gtt`. `analyze.py` recomputes the GPU memory summary from the stored raw
   samples of attempts whose collector already recorded memory, so a parsing fix
   reaches an existing series without re-measurement. On the hardware path the per-client
   sum counts surfaces shared between the player's own VA-API and Vulkan clients twice;
   reports state this next to the column.
3. Use equal minimum warmup and pre-read the file before spawn. Run both players
   XWayland fullscreen, VLC dummy/OpenGL/VA-API and Fastiplayer controls/Vulkan/
   VA-API. Record actual geometry and display modes at both boundaries. Audio stays
   active; muted gain does not disable decode/output. Keep ordinary animations and
   error logging. Record any difference from historical configuration explicitly.
4. Quality RPCs run immediately adjacent to the CPU window. Their start/end times
   and CPU-boundary skew are retained; asynchronous counters cannot be perfectly
   simultaneous. Fastiplayer trace handoffs have separate byte-offset/time brackets.
   VLC frames displayed/lost and audio buffers are different metrics. None proves
   physical scanout or equal smoothness. Quiet uses `info` Fastiplayer logging and
   default VLC verbosity; diagnostic adds handoff trace / VLC `-vv`. Both retain
   media/backend/progress checks. Pair diagnostic with quiet as observer controls;
   quiet alone cannot qualify Fastiplayer cadence. Fastiplayer audio evidence is a
   running PipeWire output plus resumed audio and advancing audio-driven media clock,
   not a measurement of audible PCM at speakers.
5. Start at **3 alternating paired repetitions** per scenario and mode. Reverse
   player order and quiet/diagnostic order on alternating repetitions. Evaluate
   only after 3, 6, 9 pairs. Extend an INCONCLUSIVE cohort to 6 and then 9; stop
   at 9 regardless of result. No reruns chosen by a favorable CPU value.
6. For paired differences `Fastiplayer - VLC`, report mean, standard deviation and
   `mean ± (10 * sample_sd / sqrt(n) + resolution_floor)`. The floor is
   `200 / CLK_TCK / shortest_window` percentage points, conservatively retaining
   the lower-resolution process-counter uncertainty even for tree CPU.
   Student t=10 has two-sided tail <0.01 for df>=2 (for df=2, tail is
   `1 - 10/sqrt(102)`). A union bound over three looks is <0.03 **per scenario/mode**,
   conditional on independent approximately normal paired run errors. This is a
   conservative model-based interval, not a distribution-free or whole-matrix
   confidence guarantee. Show raw spread and drift; a violated model invalidates
   the inferential claim. Do not summarize scenarios into one averaged victory.
7. Upper interval <0: **WIN**; lower >0: **WORSE**; otherwise **INCONCLUSIVE**.
   Missing/invalid pairs: **NOT TESTED**. With no authorized nonzero loss margin,
   finite noisy data cannot establish exact equality: **PARITY is not emitted**.
   A quality limitation remains separate even when a resource-only result is clear.
8. Compare future memory against Fastiplayer's own matching original/step-before,
   separately for steady mean and sampled maximum. No permitted systematic growth
   is built in. A reproducible positive shift requires owner discussion; an interval
   crossing zero is uncertainty, not permission for additional memory. In session 00
   these measurements establish the baseline; they cannot prove absence of a future
   regression. Same-build repetitions report noise before optimization claims.

## Measurement protocol, version 2

Introduced 2026-10-21 for the CPU-parity hardware qualification (session 06 of
`user/cpu-parity/`). Version 1 numbers above and in the 2026-09-08 baseline are
**not recalculated**; a v1 and a v2 series are never mixed in one comparison
(`compare_snapshots.py` already refuses different observer tool hashes).
Everything not listed here is unchanged from version 1.

| # | Version 1 | Version 2 | Reason |
|---|---|---|---|
| 3 | Reference: VLC `--intf dummy`, OpenGL, VA-API | Reference: **mpv** (`--no-config --hwdec=auto --fs --volume=0`, JSON IPC for properties; default OSC interface and `gpu-next` output). mpv keeps hardware decoding off by default; `hwdec=auto` is its documented way to enable it and the only deviation from defaults. The actual hwdec (on the Radeon 780M: `vulkan`, Vulkan Video on the same VCN block, zero-copy) is recorded from `hwdec-current` at both boundaries; `*-copy` and software decoding are rejected | Owner decision 2026-10-21: VLC 3.0.24 has no hardware AV1 decoding on this host (default dav1d is software; forcing avcodec fails), so it cannot be the reference for a four-codec hardware matrix. The earlier decision "VLC with Qt" is superseded. VLC support stays in the code for version 1 history |
| 4 | VLC frames lost / buffers | mpv `frame-drop-count` + `decoder-frame-drop-count` deltas (must be 0), `vo-delayed-frame-count` recorded; mpv path + inode, `time-pos` progress, PipeWire stream running | Same evidence classes as v1 |
| 3 | Fastiplayer codecs H.264, HEVC, AV1 | adds **VP9** (`VA-API VP9` backend proof) | Matrix covers four codecs |
| 2 | GPU: memory counters only, N/A total | adds **GPU engine busy time** per player tree: `drm-engine-*` nanoseconds per DRM client from fdinfo, max over FD aliases, summed over clients present at both window ends, divided by the interval between those reads. Stored as a separate `gpu` block; never added to CPU | Session exit criterion "GPU gfx not above the reference player" |
| 4–5 | Quiet and diagnostic both 3 → 6 → 9 pairs | **quiet only** follows 3 → 6 → 9 with the same stopping rule; diagnostic runs `diagnostic_repetitions` pairs (default 1) in the first round, used for the quality gate and as a descriptive observer control | Owner decision: 16-scene matrix; diagnostic CPU is never a published result |
| 4 | Quality evidence inspected manually | **Automatic quality gate** after the diagnostic round: Fastiplayer unique `(PTS, render generation, decoded generation)` identities ≥ **99 %** of `source fps × log-window seconds`; the reference player's lost-frame counters (mpv drops; VLC `frames_lost` for v1) = 0 in every valid attempt. Any FAIL stops the series (exit 2) before CPU cohorts are completed. A scene without a Fastiplayer diagnostic count is NOT TESTED, never PASS | Owner decision: a CPU WIN with lost frames is not accepted |
| 6–7 | WIN / WORSE / INCONCLUSIVE | unchanged statuses; quiet verdicts also carry `within_exit_band` = WIN, or INCONCLUSIVE with the whole interval inside ±0.5 percentage points. The same paired rule is applied separately to GPU gfx busy % (floor 0, nanosecond counter) | Session exit criterion; still **no PARITY** label |
| — | All scenarios measure both players | a scenario may set `players` (e.g. `["fastiplayer"]` when the reference cannot play a profile on this host); its CPU verdict is NOT TESTED with the reason, measured 3 times descriptively | Main10/P010 profile |
| plan | no version field | the plan must declare `"protocol_version": 2`; `collect.py` records `protocol_version` in every attempt | Prevents silent mixing |

**Known limitation of version 2 (found in session 06).** Both players are made
fullscreen by a KWin script, but mpv also enters fullscreen itself (`--fs`).
Under XWayland, winit reports `window.fullscreen()` only for fullscreen set by the
application, so Fastiplayer does not know it is fullscreen. It keeps its title
bar and player controls visible and never auto-hides them, while mpv hides its
OSC. Resource numbers therefore include Fastiplayer UI work a user would not see.
Changing how Fastiplayer enters fullscreen is a protocol change (version 3)
requiring an owner decision; see `user/cpu-parity/results/06.md`, diagnosis D2.
Version 3 below resolves it.

## Measurement protocol, version 3

Introduced 2026-10-22 (session 06b of `user/cpu-parity/`). A v2 and a v3 series are
never mixed in one comparison: `run_series.py` refuses a plan that does not declare
version 3, and the changed collector hashes force a new series directory anyway.
Everything not listed here is unchanged from version 2.

| # | Version 2 | Version 3 | Reason |
|---|---|---|---|
| 3 | Fastiplayer command: `fastiplayer <media>` | `fastiplayer --fullscreen <media>`: the player enters fullscreen itself, through the same path as its fullscreen button, so title bar and controls auto-hide as they do for a user | Owner decision 2026-10-21 (diagnosis D2): the flag mirrors `mpv --fs`; simulating a double click through uinput and detecting the window manager's fullscreen were rejected |
| 3 | KWin script makes both windows fullscreen | **unchanged** for both players; it also provides the `_NET_WM_STATE_FULLSCREEN` window proof | Same window conditions for both players |
| plan | `"protocol_version": 2` | `"protocol_version": 3`; the version lives once in `host.py` (`PROTOCOL_VERSION`) for the collector, the series runner and the summary | Prevents silent mixing |

**Software decode series (session 08 of `user/cpu-parity/`).** Version 3 also covers
the software path through two optional plan fields in `common`. Plans without them
behave exactly as before (hardware decode, ±0.5 percentage-point exit band):

| Field | Values | Effect |
|---|---|---|
| `decode` | `hardware` (default) / `software` | mpv gets `--hwdec=no` instead of `--hwdec=auto`, and `hwdec-current` must be `no` at both boundaries. Fastiplayer's software path comes from its config (`preferred_backend = "software"`); the evidence must show `plan="ffmpeg-host-upload-wgpu"` (or `plan="ffmpeg-host-upload-gles2"` on the OpenGL ES 2.0 renderer) and no VA-API trace. Hardware proof on the GLES2 renderer needs `plan="vaapi-dmabuf-gles2"` and the decoder contract `VA-API EGL DMA-BUF` instead of `VA-API Vulkan DMA-BUF`; traces of both renderer families in one attempt prove nothing, and `evidence.renderer_family` records which renderer drew the attempt. The frozen version 1 VLC command supports hardware only, so `vlc` + `software` is a plan error |
| `reference_losses` | `stop` (default) / `annotate` | `stop`: lost frames of the reference fail the scene and stop the series (session 06 rule). `annotate`: the scene gets `REFERENCE_LOSSES`, the series continues, and the CPU table must be published with the caveat "with reference frame losses" and without any superiority claim (session 08 plan, T480s report rule). Fastiplayer's own losses are `FAIL` under either policy |
| `exit_band_percentage_points` | positive number, default `0.5` | Width of the "INCONCLUSIVE inside ±band" exit criterion recorded as `within_exit_band`. Session 08 uses `1.0` for software scenes (session plan). One value per series |

The owner chose mpv as the software reference as well (2026-10-22), so hardware and
software reports compare against the same player.

**Renderer, window mode, compositor and three players (session 10 of
`user/gles2-renderer/`).** Version 3 also qualifies the OpenGL ES 2.0 renderer against
our wgpu renderer and against mpv in GL mode. Every field below is optional, and a plan
without these fields is analysed exactly as before: the old keys of the summary of the
`cpu-parity-08b` series stay byte-for-byte equal, and new keys are only added.

| Field | Where | Values | Effect |
|---|---|---|---|
| `kind` | player entry | `fastiplayer` / `mpv` / `vlc`, default = player name | Selects the command, the evidence and the quality gate. Player names are free identifiers `[a-z0-9-]+` |
| `subject` | plan top level | player name, default `fastiplayer` | The Fastiplayer whose quality and CPU are judged. All other players are references; `analyze.py` writes one pair "subject × reference" per reference into `pairs` of each cohort. The old top-level `reference_player`/`resource_verdict`/`gpu_gfx_verdict` hold the first reference by name |
| 2 or 3 players | `players` | — | Two players keep the AB/BA order. Three rotate cyclically per repetition (a Latin square: after three repetitions each player has held each position once). The 3/6/9 stop rule stops a scene only when the verdict of **every** pair is terminal |
| `renderer` | Fastiplayer entry | `gles2` / `wgpu` | The renderer comes from the entry's own `config` (`[render] profile = "opengles"` or `"auto"`). The attempt is invalid unless `evidence.renderer_family` equals this value, so a wrong config cannot silently swap the subject |
| `args` | mpv entry or scenario | list of `--option` strings | Appended after `--no-config --hwdec=… --fs --volume=0` and before the IPC socket; mpv takes the last occurrence of an option. `--input-ipc-server` is rejected |
| `expect` | mpv entry | `{property: value}` of boundary properties | For example `{"current-vo": "gpu", "current-gpu-context": "wayland", "hwdec-current": "vaapi"}`. Every property must equal its value at both window boundaries. An unknown property name is a plan error before launch |
| `window_mode` | `common` or scenario | `xwayland` (default) / `wayland` | `xwayland` withholds `WAYLAND_DISPLAY` as before and proves the window through xprop/XRes. `wayland` passes the collector's `WAYLAND_DISPLAY` and proves the window through a KWin script (`kwin.py`): exactly one KWin window of the PID, fullscreen, of `display_size`, and **no** X11 window of the PID. `capture` is unsupported under `wayland`. All players of a cohort must use the same mode |

The compositor is measured in every new attempt. Before the player starts, the
collector finds `kwin_wayland` and `Xwayland` of the current user by `comm` and keeps
their identity (PID + `starttime`). Each sample reads their `utime+stime`. A
compositor process that restarts, vanishes or appears during the window makes the
attempt invalid. `kwin_wayland` is always required, and `Xwayland` is required under
`xwayland`. Under native Wayland a live Xwayland is still counted as an honest part
of the cost. The metrics are `compositor_cpu_percent` (with a per-name breakdown) and
`tree_plus_compositor_cpu_percent`. `analyze.py` gives each pair a second CPU verdict,
`compositor_resource_verdict`, on the sum. It uses the same Student interval and exit
band. The quantisation bound adds one `/proc` tick per compositor process of both
attempts. Old series have no such fields, so their verdict is `NOT TESTED` with the
reason "compositor CPU not recorded".

A reference of kind `fastiplayer` (for example wgpu against a GLES2 subject) passes
the same unique-frame gate as the subject (≥ 99 % of source fps × window). It does not
get mpv's "0 dropped frames". Its failure counts as a reference loss under
`reference_losses`.

A/V sync is reported in `quality_gates[scene].av_sync`, separately from the frame-gate
status. mpv reports `avsync` (seconds, read at both boundaries); a reference attempt
passes when the largest absolute value is ≤ 45 ms. That is the strictest detectability
bound of ITU-R BT.1359, applied to both signs. Fastiplayer has no honest A/V offset
source: MPRIS `Position` is the media clock, and the render trace carries no audio
clock. The subject's A/V therefore stays `NOT TESTED` with that reason and is never
inferred.

Example: a native Wayland hardware series with three players (paths shortened).

```json
{
  "protocol_version": 3,
  "diagnostic_repetitions": 1,
  "subject": "gles2",
  "common": {"warmup_seconds": 12, "duration_seconds": 20, "interval_seconds": 1,
             "display_size": [1920, 1080], "window_helper": ".../window-owner",
             "window_helper_sha256": "…", "window_mode": "wayland", "decode": "hardware",
             "snapshot": "gles2-10"},
  "players": {
    "gles2": {"kind": "fastiplayer", "renderer": "gles2", "binary": ".../fastiplayer",
              "binary_sha256": "…", "config": ".../gles2-hardware.toml", "config_sha256": "…"},
    "wgpu": {"kind": "fastiplayer", "renderer": "wgpu", "binary": ".../fastiplayer",
             "binary_sha256": "…", "config": ".../wgpu-hardware.toml", "config_sha256": "…"},
    "mpv-gl": {"kind": "mpv", "binary": "/usr/bin/mpv", "binary_sha256": "…",
               "args": ["--vo=gpu", "--gpu-api=opengl"],
               "expect": {"current-vo": "gpu", "current-gpu-context": "wayland", "hwdec-current": "vaapi"},
               "config": ".../gles2-hardware.toml", "config_sha256": "…"}
  },
  "scenarios": [{"scenario": "h264-1080p60-wayland", "codec": "h264",
                 "media": ".../1080p60_h264-high_40mbps_aac.mp4", "media_sha256": "…"}]
}
```

The collector copies `config` for every player, so the mpv entry carries one too; mpv
itself runs with `--no-config`.

**Measurement condition in version 3.** Keep the pointer away from the player's
panels (for example in the middle of the screen) and do not touch the mouse. A
pointer resting over the title bar or the bottom controls holds them visible by
design (fullscreen auto-hide, UX14). Under native Wayland a window that becomes
fullscreen under a resting pointer receives the pointer position at once, so the
hold applies without any movement; under XWayland (the protocol default) it was
not observed in session 06b.

Matrix media are generated by `make_matrix.py` from one owner-selected master
(4 resolutions/frame rates × H.264/HEVC/AV1/VP9, "heavy but real-world" bitrates at
the upper end of each codec level, 2-second GOP, 8-bit 4:2:0, AAC or Opus audio).
The script verifies each output with ffprobe and writes `manifest.json` with hashes and
actual bitrates. Media stay outside Git; reports publish the recipe and hashes.

```sh
python3 docs/benchmarks/tools/local/make_matrix.py /absolute/master-4k60.mp4 \
  /absolute/benchmark-matrix --start 300 --duration 45 --jobs 4
```

`--preset` selects the scene set (default `matrix`, the 17 scenes above). The
`t410` preset holds the old-hardware scenes of the GLES2 plan
(`user/gles2-renderer/results/00.md`). It has real-world H.264 720p/1080p at 30/60 fps,
a stress ladder (H.264 40 Mbit/s, HEVC/VP9/AV1 720p–1080p60), and MPEG-2 576i/1080i
encoded interlaced (top field first, PAL 16:9 SAR, MPEG-TS + MP2, level-limited VBV).
`t410-hdr` is one HEVC Main10 HDR10 scene and needs a PQ master. An SDR master is
rejected before encoding, because the generator stamps PQ tags on the frames itself.

```sh
python3 docs/benchmarks/tools/local/make_matrix.py --preset t410 --start 300 --jobs 4 \
  /absolute/master-4k60-sdr.mp4 /absolute/benchmark-t410
python3 docs/benchmarks/tools/local/make_matrix.py --preset t410-hdr --start 300 --jobs 1 \
  /absolute/master-4k60-hdr10.mp4 /absolute/benchmark-t410-hdr
```

Record OS/kernel/CPU/GPU/Mesa/libva/PipeWire/display protocol/mode, tool versions,
AC/governor/EPP/background conditions, exact build command/toolchain/features,
source commit and complete patch or source archive, binary and Cargo.lock hashes,
config, scripts and durable media location/hashes. Keep original historical evidence
unchanged. Name this initial snapshot `original`; `quality-fixed`, `step-before` and
`current` require actual future snapshots and must not be invented.

A successful collector run is `VALID_RESOURCE_OBSERVATION`, **not** a playback
quality certificate. Inspect handoff identities, VLC losses, progression rates,
window evidence, errors, observer controls and background before session acceptance.
Large noise, same-sample repeated frames, missing coverage and unavailable devices
remain visible limitations. Do not claim that this benchmark has established equal
smoothness.

Kernel accounting semantics: [cgroup v2 CPU interface](https://docs.kernel.org/admin-guide/cgroup-v2.html#cpu).
Transient-unit lifecycle: [systemd control-group interface](https://systemd.io/CONTROL_GROUP_INTERFACE/).
