# Frame cadence diagnostics

Related investigation: [#13](https://github.com/Bogdan7c/fastiplayer/issues/13).
Repeated surface handoffs can come from a previously selected frame, a busy
resource lookup, or a lower video frame rate than the display refresh rate.
These events distinguish the first two without changing playback scheduling.

Enable the narrow trace targets for a **bounded diagnostic attempt**:

```sh
RUST_LOG='info,fastiplayer::frame_cadence=trace,fastiplayer::video_render_acceptance=trace,fastiplayer::render_frame_timing=trace' \
  target/release/fastiplayer /absolute/path/to/authorized-media.mp4
```

Use an isolated configuration and a timed process owner as described in the
[local measurement protocol](tools/local/README.md). Preserve all attempts and
record the binary/source/config/media hashes. Keep raw logs private: ordinary
startup events can contain local media paths. Stop the process after the planned
window; these targets do not implement log rotation or a time limit themselves.
They emit a fixed number of fields per render attempt and retain no frame history.
Leave them disabled for resource measurements; trace overhead is not assumed zero.

Within the single render thread, correlate events in order:

| Event | Owner | Meaning |
| --- | --- | --- |
| `video frame prepared for surface` | `app-egui::frame_prepare::submit` | Exact prepared PTS/generations, acquisition reason, texture lookup outcome and whether a video input exists, before entering the renderer |
| `surface acquire started` | `render-wgpu-shell::Renderer` | Immediately before attempting surface acquisition |
| `surface acquire finished` | `render-wgpu-shell::Renderer` | Acquisition/recovery returned; `acquired=false` is a dropped attempt |
| `current video frame submitted to surface` | `app-egui::frame_prepare::submit` | Existing handoff event behind the `Presented` gate |
| `render frame stage timings` | `app-egui::frame_prepare::timing` | Existing durations, including blocking surface wait |

Optional identity values on the prepared event are `None` when there is no frame.
Frame identity uses PTS and render/decode generations, never allocation identity.
Surface events also cover UI-only frames; correlate with the preceding prepared
event rather than treating every acquisition as a video frame. A renderer failure
may have a successful acquisition without a handoff. Outdated-surface recovery is
included within one acquisition interval. Trace timestamps bracket the calls and
include tracing overhead; they are not GPU timestamps or physical scanout evidence.

For a separate detailed attempt, existing module filters can additionally expose
`player_core::session::tick::presentation_scheduler=trace` (selection and waits),
`player_core::session::tick::video_decoder_io=debug` (drained decoded frames),
`player_core::worker::runtime_publish=debug` (periodic pressure/drop summaries),
and `video_vaapi::decoder_thread::resource_provider=trace` (post-GPU release).
These produce more output and must not be pooled with quiet CPU measurements.
Decode-drain time is an upper bound on decoder readiness, not its exact completion.
Resource-release handles alone do not establish a PTS-to-release identity mapping.

A new frame selected during surface acquisition does not by itself prove that
moving selection is a correct fix. Check the prepared acquisition/lookup reason,
queue/decode readiness, clock progression, missing PTS intervals, generations and
release pressure. A deterministic regression should reach changing rendered pixels
under controlled clock/acquisition timing before approving a scheduler change.
Do not equate expected repeats for 24/30 fps video on a 60 Hz display with a missed
60 fps update. Submission counts alone do not establish equal smoothness.

## Deterministic snapshot characterization

Run outside the filesystem/device sandbox:

```sh
cargo test -p app-egui frame_prepare::shared_frame_materialization::cadence_tests:: --locked
```

`frame_prepare/cadence_tests.rs` publishes two distinct 2×2 YUV resources in a
fixed order. The production materializer, `PreparedVideoFrame` input conversion
and WGPU video renderer draw into a 64×64 offscreen attachment. Test-only
readback proves that retaining the early snapshot repeats the previous pixels,
while preparing after publication changes the pixels. Shared leases survive
publication and drawing; the harness waits for GPU completion before dropping
them and checks exactly one submitted release per resource.

This passing characterization intentionally documents current snapshot behavior.
It is **not** the failing regression required to approve a render-loop fix: the
publisher is scripted, there is no real worker/media clock or blocking swapchain,
and the fake release sink does not test the decoder's GPU release bridge. It also
does not test Busy fallback or attribute the historical 1397/1800 failure. A future
integration regression must drive the production selection/acquisition ordering
and assert fresh rendered output when the next eligible frame is published during
acquisition. Changing the snapshot characterization's expected pixels alone would
not provide that regression.
