# Local CPU / memory / playback evidence

These tools run an independent local series. They do not replace or rewrite the
published [T480s tools](../t480s/collect.py). Production playback code is unchanged.
Raw output contains local paths and runtime logs: keep the series outside tracked
files and publish only reviewed, anonymized evidence.

## Ownership and requirements

- `service.py` owns one transient **user** systemd service per attempt. The player
  and all descendants enter its cgroup before exec. No CPU/memory/power limits or
  personal settings are changed. Cleanup targets only that unit and records exit
  status before removing it.
- `host.py` reads process `/proc/PID/stat`, `smaps_rollup`, cgroup v2 `cpu.stat`,
  separate charged cgroup memory, and background/power observations.
- `evidence.py` checks the exact media inode/current input, actual backend, window,
  advancing media time and audio-consumer evidence. It reuses the existing T480s
  render-event and VLC-counter readers. Fastiplayer MPRIS intentionally omits URLs.
- `window_owner.c` queries XRes local client PID and window geometry, including VLC
  windows that do not expose `_NET_WM_PID`.
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
python3 docs/benchmarks/tools/local/smoke.py /absolute/fastiplayer-spec.json /absolute/new-smoke
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
