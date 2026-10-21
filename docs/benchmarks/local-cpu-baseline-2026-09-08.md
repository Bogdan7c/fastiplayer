# Local CPU baseline — AMD, 2026-09-08

**Fastiplayer used more CPU than VLC in all five local scenarios, in both logging
modes. Its observed process RSS/PSS was lower. Equal smoothness is not established.**
This is session 00 measurement infrastructure and an initial `original` snapshot,
not an optimization or a replacement for the historical T480s benchmark.

Task: [#11](https://github.com/Bogdan7c/fastiplayer/issues/11);
tracking goal: [#10](https://github.com/Bogdan7c/fastiplayer/issues/10);
implementation: [#12](https://github.com/Bogdan7c/fastiplayer/pull/12).

## Provenance and scope

- Production source: `0cfe62ef360f15e8bc3aad9190d5a7da9f098921`.
- Measured observer/tools: `7a436e3c`; the full hash is in the
  [compressed per-run evidence](local-cpu-baseline-2026-09-08.json.gz).
- Fastiplayer release SHA-256:
  `b94e278e49cb5fb725c89eb9cfb0460346cf8ed4a85189219b733714c61a5e91`.
  `cargo build --release -p app-egui --locked` confirmed that hash. No production
  source changed; source/tool archives and the exact executable are retained.
- AMD Ryzen 7 7840HS / Radeon 780M, Linux 7.2.2-1-cachyos, Mesa 26.2.2,
  KDE/KWin Wayland desktop with both players forced to **XWayland fullscreen**,
  1920×1080 at reported 59.96 Hz (nominal 60 Hz).
- VLC 3.0.23_2-13.1, libva 2.24.1, PipeWire 1.6.8, FFmpeg 9.0.1,
  Python 3.14.7, systemd 261.2, XWayland 24.1.13, libxcb 1.17.0;
  Rust 1.96.0, default app-egui release features.
- VLC dummy interface, OpenGL and confirmed VA-API; Fastiplayer controls,
  Vulkan and confirmed VA-API DMA-BUF. Audio decoding/output remained active at
  zero gain. This is not a comparison of identical UI/backend implementations.
- Fresh XDG config/data/cache/state per attempt; media fully pre-read before
  spawn. Minimum warmup 12 seconds plus recorded boundary RPC time.
  Synthetic windows: 30 seconds. Real-content windows: 15 seconds. Samples: 1 Hz.
- The [historical hardware config](tools/t480s/hardware-template.toml) was copied
  with only `reduced_motion = false`, retaining ordinary animations. Personal
  configuration and power settings were not changed. This is a new baseline,
  not a retrospective alteration of historical numbers.
- Governor/EPP remained performance/performance. Observed one-minute load average
  ranged 0.21–1.11; the available CPU-associated thermal zone ranged 51–56 °C.
  This was a live desktop, not a controlled idle system. No compilation or
  profiling overlapped scoring. AC online telemetry was unavailable on this host.

The two original synthetic media files were regenerated with exact historical
hashes: H.264 `58074d29d884ca434053e13d859226559bdcd0e9ecd4c58aa84b9c0e1ba1a5c1`,
HEVC `43d2d33c42be3c852d99f9243e7ccaa37e43e5f0469e4dcc84fa512213a257a6`.
The existing [fixture generator](tools/t480s/make_fixtures.py) was reused.
HEVC is an upscaled simple pattern, not natural 4K complexity.

The real-content corpus is small and private: ~24 fps animation footage converted
from AV1 to a common H.264/AAC input, ~30 fps local video, and 60 fps authored
application/demo footage. Source/derived files, recipes and hashes are retained
privately; media is not redistributed. These cases are not independently
reproducible without the private inputs and are not a generally representative
video corpus. All publicly exported paths/names are neutral scenario labels.

## Resource results

There were **60/60 valid resource observations**: five scenarios × two players ×
two logging modes × three repetitions. Player and mode order alternated.
All ten scenario/mode comparisons returned **WORSE** for Fastiplayer CPU at the
first planned evaluation, so the predeclared extension to 6/9 pairs was not used.
This does not convert the quality limitations below into a pass.

CPU is percent of **one logical core**; 100% = one core. The table uses independent
cgroup process-tree CPU and shows mean [minimum–maximum] across three quiet runs.
Main-process utime+stime/CLK_TCK, actual timestamps and both CPU formulas are also
preserved in the per-run data. Compositor/audio-server CPU is outside this scope.

| Scenario | Fastiplayer CPU | VLC CPU | Paired difference interval, percentage points |
| --- | ---: | ---: | ---: |
| Synthetic H.264 1080p60 | 7.927 [7.831–7.986] | 3.598 [3.359–3.788] | +2.576 to +6.083 |
| Synthetic HEVC 4K60 | 10.541 [10.368–10.722] | 4.690 [4.514–4.856] | +3.998 to +7.704 |
| Real H.264 1080p ~24 fps | 6.081 [6.067–6.098] | 2.162 [2.129–2.197] | +3.599 to +4.239 |
| Real H.264 1080p ~30 fps | 6.556 [6.457–6.670] | 2.825 [2.798–2.841] | +2.848 to +4.615 |
| Real H.264 1080p60 | 7.828 [7.785–7.860] | 3.815 [3.482–3.984] | +2.427 to +5.599 |

Memory is the mean of each run's sampled steady-window mean, in MiB. Raw sampled
maxima and per-run spread are retained in the evidence. The two columns within a
player are RSS / PSS; they are alternative views, not quantities to add.

| Scenario | Fastiplayer RSS / PSS | VLC RSS / PSS |
| --- | ---: | ---: |
| Synthetic H.264 1080p60 | 111.26 / 68.72 | 146.53 / 82.60 |
| Synthetic HEVC 4K60 | 112.22 / 69.66 | 312.37 / 248.51 |
| Real H.264 1080p ~24 fps | 110.13 / 67.44 | 143.65 / 79.75 |
| Real H.264 1080p ~30 fps | 116.35 / 73.71 | 192.90 / 128.86 |
| Real H.264 1080p60 | 111.59 / 68.97 | 157.81 / 93.72 |

These establish Fastiplayer's own baseline, not a permitted future RAM increase.
Systematic growth in a later matching before/current comparison requires owner
review; no +5% allowance is built in. Sampled maxima can miss short-lived peaks.
Charged cgroup/kernel memory and DRM memory counters were recorded privately
separately. DRM FD aliases are deduplicated per device/client, but shared
allocations across clients are not identifiable: a whole-GPU total is **N/A**.
Do not add these counters to process RSS/PSS or claim lower total system memory.

## Noise, decision rule and observer controls

The [frozen protocol](tools/local/README.md#measurement-protocol-version-1) evaluates
only at 3/6/9 pairs and caps a cohort at 9. For paired CPU differences Fastiplayer
minus VLC, the interval is mean ± 10×sample_sd/√n plus a process-resolution floor.
It is conditional on approximately normal independent paired-run errors. The
conservative Student factor covers three planned looks per scenario/mode; it is
not a distribution-free or whole-matrix confidence guarantee. Large noise or an
interval crossing zero means INCONCLUSIVE, not PARITY. No nonzero loss margin was
approved, so exact PARITY is not emitted. No scenario averaging hides a loss.

Same-build quiet-run CPU standard deviations, percentage points:

| Scenario | Fastiplayer sample SD | VLC sample SD |
| --- | ---: | ---: |
| Synthetic H.264 1080p60 | 0.084 | 0.219 |
| Synthetic HEVC 4K60 | 0.177 | 0.171 |
| Real H.264 1080p ~24 fps | 0.016 | 0.034 |
| Real H.264 1080p ~30 fps | 0.107 | 0.023 |
| Real H.264 1080p60 | 0.038 | 0.289 |

The collector itself used 0.324–0.447% of one core during the sampled windows,
separately from the player cgroup. This measures its own CPU, not every indirect
scheduler/driver perturbation it may cause. Fastiplayer diagnostic-minus-quiet
mean CPU differences ranged +0.085 to +0.405 percentage points; all corresponding
companion-control intervals crossed zero. VLC companion controls were also
INCONCLUSIVE. These controls do **not** establish zero observation cost. Quiet
resource measurements and diagnostic quality evidence therefore remain separate.

## Quality and exclusions

CPU/quality boundary skew was 20.0–44.4 ms and is retained per attempt. VLC media
time, displayed frames and audio buffers progressed; its frames_lost/buffers_lost
counter deltas were zero throughout the scored series. Fastiplayer's same-PID
MPRIS position advanced with a running PipeWire audio output and resumed audio;
its media-progress / CPU-window-duration ratios were 1.001–1.003. The quality
reads enclose the CPU window, explaining the small excess; this is not a playback
speed claim. RPC timing also does not bound a counter's internal update batching.
This is not audible-speaker PCM or physical scanout evidence.

Fastiplayer diagnostic handoffs / unique (PTS, render generation, decoded
generation) identities, in repetition order:

| Scenario | Repeat 1 | Repeat 2 | Repeat 3 |
| --- | ---: | ---: | ---: |
| Synthetic H.264 1080p60 | 1800 / 1799 | 1800 / 1800 | 1800 / 1798 |
| Synthetic HEVC 4K60 | 1800 / 1797 | 1800 / 1774 | 1800 / 1799 |
| Real H.264 1080p ~24 fps | 900 / 360 | 900 / 361 | 900 / 360 |
| Real H.264 1080p ~30 fps | 900 / 450 | 901 / 450 | 900 / 450 |
| Real H.264 1080p60 | 901 / 809 | 900 / 826 | 900 / 898 |

At 24/30 fps, repeated handoffs on a ~60 Hz display are expected; they must not be
mislabelled as the same anomaly as reduced unique identities on a 60 fps source.
The real 60 fps case (809/826/898 identities) and the HEVC spread remain open quality
observations, not diagnoses. Handoff identities and VLC frame-loss counters are
not equivalent. Quiet Fastiplayer does not emit handoff traces. A separate real
window smoke showed changing image content, but it does not qualify cadence or
prove equal smoothness. **No `quality-fixed` snapshot exists.**

Main10/P010 was attempted before freezing the scored matrix. Fastiplayer's pilot
confirmed P010 zero-copy and advancing output. VLC's `glconv_vaapi_x11` failed EGL
image/video-output creation and fell back. This hardware pair is **NOT TESTED /
PROFILE UNAVAILABLE**, not a zero-CPU or hardware/software-equivalent result.

An initial series at tooling e4e5f3b9 stopped on its second attempt because the
VLC Unix socket path exceeded AF_UNIX's limit. Its single valid observation and
failed attempt are retained separately and excluded from this series. The root
cause was fixed in 7a436e3c by placing only ephemeral IPC in a short private runtime
directory; a real VLC long-artifact-path regression and cleanup check passed.
Development pilots and their failures remain in private artifacts, not mixed into
scored cohorts.

## Validation and reproduction

- 9 local functional/report tests PASS: real CPU/PSS, reaped children, early exit,
  invalid media/timing, no false parity, actual before/current reporting, units and
  incompatible-protocol rejection.
- 2 unchanged T480s collector tests PASS.
- Real window/render smoke PASS; intentionally wrong software backend INVALID;
  long-path VLC playback and temporary IPC cleanup PASS.
- `bash scripts/pre-pr-checks.sh` PASS on tooling 7a436e3c. An initial existing
  instance-lock test failure is retained; isolated and subsequent full runs passed.
- All 21 GitHub checks passed on tooling 7a436e3c. The local desktop/cgroup smoke is
  separate from hosted CI and is not implied by hosted Rust tests.

Use the [tool guide](tools/local/README.md) for exact build/test/collector/series
commands and the JSON input contract. The compressed evidence preserves per-run
spec hashes, CPU samples, memory samples, observer CPU, quality identities and
exit outcomes, sufficient to recompute the published arithmetic. Recoverable
source archives, saved binaries/runtime, Cargo.lock, toolchain/config, original
and derived media, all raw logs and restoration instructions are retained in the
owner's durable private artifact directory. Public sources permit rebuilding;
private real-media inputs and the complete host installation are not distributed.
Historical reports and the landing-page benchmark claims remain unchanged.
