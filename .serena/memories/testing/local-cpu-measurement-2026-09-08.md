# Local CPU/memory/playback measurement — session 00

Branch `test/11-cpu-measurement`, PR https://github.com/Bogdan7c/fastiplayer/pull/12; task #11, tracking #10. Check merge status before assuming available on main. No production Rust or historical T480s tools/results changed. Production original: `0cfe62ef360f15e8bc3aad9190d5a7da9f098921`; release SHA-256 `b94e278e49cb5fb725c89eb9cfb0460346cf8ed4a85189219b733714c61a5e91`. Measured tooling: `7a436e3c`; source archives/binary/config/media recovery are retained privately.

Authoritative public entry points: `docs/benchmarks/tools/local/README.md`, `docs/benchmarks/local-cpu-baseline-2026-09-08.md`, compressed numerical evidence `docs/benchmarks/local-cpu-baseline-2026-09-08.json.gz`. Local handoff: `user/cpu-optimization-plan/results/00.md`; raw/recovery root: `user/cpu-optimization-plan/artifacts/00/`. Never force-add private media/paths/logs.

## Ownership / invariants

- `service.py` owns one transient user systemd service/cgroup including all descendants and captures exit state before cleanup. No power/resource limits or personal settings change.
- `host.py`: process utime+stime/CLK_TCK and separate cgroup microsecond CPU; RSS/PSS means and sampled maxima; separate charged cgroup/kernel memory; per-DRM-client fdinfo deduplicates FD aliases but NEVER sums across clients sharing allocations. Whole-GPU total remains N/A.
- `evidence.py`: exact opened media inode + same-PID MPRIS (URL intentionally omitted), actual backend, advancing playback, running PipeWire audio output. ALSA node ownership follows client.id -> Client application.process.id. Reuses T480s fullscreen/render-event/VLC-counter readers.
- VLC lacks _NET_WM_PID: `window_owner.c` uses XRes local PID and geometry. Only ephemeral VLC IPC uses a short private XDG_RUNTIME_DIR subdirectory; long artifact paths otherwise fail AF_UNIX limits.
- `collect.py` preserves explicit INVALID attempts and cleanup failures. `run_series.py` freezes plan/tool hashes, alternates player/mode order and stops on invalid attempts. `analyze.py` separates resource status from quality. `compare_snapshots.py` requires actual matching-protocol/observer/repetition snapshots and explicit CPU percentage-point versus KiB units.

## Protocol / tests

Linux cgroup v2/user systemd, KDE KWin XWayland, PipeWire, Python stdlib, VLC3, ffprobe, qdbus6/pw-dump/xprop/xrandr, libxcb/libxcb-res C helper; spectacle for window smoke. Commands outside sandbox:
- `python3 -m unittest discover -s docs/benchmarks/tools/local -p 'test_*.py' -v` (9 functional/report tests).
- `cc -Wall -Wextra -Werror -O2 docs/benchmarks/tools/local/window_owner.c -lxcb -lxcb-res -o <artifact>/window-owner`.
- `python3 docs/benchmarks/tools/local/smoke.py <fi-spec> <new-dir> --vlc-spec <vlc-spec>` (changing actual window/render output, wrong software backend INVALID, long-path VLC and IPC cleanup).
- Existing T480s collector tests stay separate; full `bash scripts/pre-pr-checks.sh` applies.

Evaluate CPU-vs-VLC only at 3/6/9 pairs, maximum 9. Conservative t=10 plus resolution interval, conditional approximately-normal independent paired-run model, per scenario/mode, not a whole-matrix guarantee. WIN/WORSE when interval strictly excludes zero; otherwise INCONCLUSIVE. PARITY is not emitted without an authorized loss boundary. Memory uncertainty never authorizes growth. Quiet/diagnostic controls and sampler CPU are separately reported; no observer-free claim.

## Completed original observations

Fresh `original-series-v2`: 60/60 valid observations, five scenarios x two players x two modes x three repeats. All ten resource comparisons WORSE for Fastiplayer CPU at first planned look; extension not needed. Quiet mean CPU FI/VLC: synthetic H264 1080p60 7.927/3.598; synthetic HEVC 4K60 10.541/4.690; real H264 ~24fps 6.081/2.162, ~30fps 6.556/2.825, 60fps 7.828/3.815 (% one core). FI process RSS/PSS lower in each case; this establishes its baseline, not permission for future growth.

Synthetic windows 30s, real 15s; minimum warmup12s + boundary RPC, 1Hz sampling, XWayland 1920x1080 59.96Hz; performance governor/EPP, live desktop. Normal animations retained via reduced_motion=false versus historical true. Exact old synthetic media hashes recovered. Real24 is a documented H264 derivative of local AV1 animation; real60 is authored app/demo footage. Small private corpus is not generally representative.

Quality remains open: FI synthetic H264 unique identities 1799/1800/1798 per 1800 handoffs; HEVC 1797/1774/1799. Real60 809/826/898 vs ~900 handoffs. At real24/30 fps, repeated ~60Hz handoffs are expected (unique ~360/450), not equivalent to the 60fps observation. VLC frames_lost/buffers_lost deltas zero, but not equivalent to handoff or scanout. Boundary skew 20–44ms. Collector CPU 0.324–0.447% one core; diagnostic-minus-quiet control intervals INCONCLUSIVE, not zero overhead.

Main10/P010: FI pilot confirmed P010 zero-copy; VLC VA-API/OpenGL EGL-image/video-output path failed/fell back, so hardware pair NOT TESTED/profile unavailable. No equal smoothness, physical scanout, audible speaker PCM, whole-GPU total or quality-fixed claim. Historical quality issue is not closed. Session 01 subsequently added opt-in cadence diagnosis on issue #13; its scoped evidence and unresolved historical-causality gate are in `mem:testing/frame-cadence-diagnostics-2026-09-09`. This does not alter the original baseline.

Initial series at e4e5f3b9 aborted on socket path limit; retained separately, not pooled into v2. One initial existing instance-lock full-suite failure was retained; isolated and later full checks passed. Current final CI/check/merge status belongs in the PR/handoff, not inferred from baseline measurements.