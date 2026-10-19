# Dev diagnostics: where does the CPU go?

Quick, non-protocol tools for the CPU-parity work (`user/cpu-parity/`). They answer
"which thread / which frame stage / which GPU engine" in about a minute, before and
after a change. They are **not** the publication protocol: no cgroup accounting, no
XWayland fullscreen, no alternating repetitions, no quality RPCs. Numbers from these
scripts go into session reports under `user/cpu-parity/results/`, never into the
landing page or `docs/benchmarks/*.md` reports.

- `thread_cpu.py PID [--warmup S] [--duration S]` — per-thread CPU (% of one logical
  CPU, from `/proc/PID/task/*/stat`), process RSS/PSS (`smaps_rollup`), per-process GPU
  engine busy (`/proc/PID/fdinfo` `drm-engine-*`, deduplicated by `drm-client-id`) and
  whole-GPU busy from sysfs. Works on any Linux process, including VLC.
- `run_pass.py label=player:mode:codec[:intf] …` — launches Fastiplayer (isolated XDG
  directories, config copied from `../t480s/{hardware,software}-template.toml`) or VLC
  (`--ignore-config`, Qt UI by default, `--avcodec-hw=vaapi|none`) on the same file,
  runs `thread_cpu.py` against it, then terminates it. Output goes to
  `benchmark-output/dev-pass/<label>/{player.log,report.txt}` (ignored by git).
- `agg_timing.py player.log …` — aggregates `fastiplayer::render_frame_timing=trace`
  lines into per-stage mean / p50 / p90 / max (`frame_total`, `ui_prepare`,
  `video_prepare`, `renderer_submit`, `queue_submit`, `surface_present`, …). Pass
  `--env RUST_LOG=info,fastiplayer::render_frame_timing=trace` to `run_pass.py` first.

Typical before/after check for a hardware-path change:

```sh
cargo build --release -p app-egui --locked
python3 docs/benchmarks/tools/t480s/make_fixtures.py ./benchmark-fixtures   # once
python3 docs/benchmarks/tools/dev/run_pass.py \
    fp-hw-h264=fastiplayer:hw:h264 fp-hw-hevc=fastiplayer:hw:hevc vlc-hw-h264=vlc:hw:h264
python3 docs/benchmarks/tools/dev/run_pass.py \
    --env RUST_LOG=info,fastiplayer::render_frame_timing=trace trace-hw-h264=fastiplayer:hw:h264
python3 docs/benchmarks/tools/dev/agg_timing.py benchmark-output/dev-pass/trace-hw-h264/player.log
```

Close other players first (Fastiplayer forwards files to a running instance over
D-Bus). The desktop stays live, so treat differences below ~0.3 percentage points
as noise and repeat the pass.
