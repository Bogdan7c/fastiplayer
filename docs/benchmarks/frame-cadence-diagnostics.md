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
| `latest video frame published` | `player-core::LatestPresentFrameHandoff` | New nonempty lease is available after unlocking the handoff slot and dropping its previous owner; PTS and render/decode generations identify this publication |
| `video frame prepared for surface` | `app-egui::frame_prepare::submit` | Exact prepared PTS/generations, acquisition reason, texture lookup outcome and whether a video input exists, after successful surface acquisition, before draw |
| `surface acquire started` | `render-wgpu-shell::Renderer` | Immediately before attempting surface acquisition |
| `surface acquire finished` | `render-wgpu-shell::Renderer` | Acquisition/recovery returned; `acquired=false` is a dropped attempt |
| `current video frame submitted to surface` | `app-egui::frame_prepare::submit` | Existing handoff event behind the `Presented` gate |
| `render frame stage timings` | `app-egui::frame_prepare::timing` | Existing durations, including blocking surface wait |

Optional identity values on the prepared event are `None` when there is no frame.
Frame identity uses PTS and render/decode generations, never allocation identity.
Surface events also cover UI-only frames; correlate acquisition with the following
prepared event rather than treating every acquisition as a video frame. Failed
acquisition reports `surface_not_acquired` and does not request a video lease. A renderer failure
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

## Production ordering regression

Linux manual acceptance in `app_instance/cadence_acceptance.rs` now drives the
real process bootstrap, AppShell, worker, configured decoder and X11 swapchain.
Supply an existing isolated baseline config and a local H.264 60fps fixture.
Run each test alone in its own process, outside the sandbox:

```sh
export CADENCE_MEDIA=/absolute/path/to/h264-60fps.mp4
export CADENCE_CONFIG=/absolute/path/to/baseline-config.toml
cargo test -p app-egui --locked -- --ignored --exact \
  app_instance::cadence_acceptance::published_before_preparation_reaches_same_surface_handoff --nocapture
cargo test -p app-egui --locked -- --ignored --exact \
  app_instance::cadence_acceptance::published_during_acquisition_reaches_same_surface_handoff --nocapture
```

The first is the passing control. The second failed at `CADENCE_FRESHNESS_DEFECT` before the correction in #15
and must pass after it. Normal contributor checks skip these explicit manual tests; a
green ordinary suite does not mean the freshness regression passes. Missing media,
display/backend failure, Busy, dropped acquisition and barrier timeouts must not
be classified as successful reproductions.

After 30 real handoffs the test subscriber parks the worker just after a new lease
is published, outside the handoff lock. Once that frame has been presented, the
control releases the worker and waits for the next publication before the next
preparation. The regression instead releases it at the next acquisition-start
event, after that pinned frame was presented. Before #15 the next input was
already prepared at this point; after #15 preparation follows acquisition. It waits for a newer
publication before allowing the normal surface call to proceed. Both then check
the actual Presented-gated handoff and matching generations. The required new PTS
must reach that handoff; an older PTS fails. Only the relative ordering is fixed,
not wall-clock times or startup PTS. Barriers time out after 3s; the application
deadline is 25s and all process owners use the ordinary shutdown path.

Three retained hardware pairs passed the control and failed the freshness check
through Ready on VA-API H.264. This establishes the early-preparation cause for
the reproduced Ready path. It does not attribute every repeat in the historical
1397/1800 run, diagnose Busy contention, prove physical scanout or qualify CPU/RAM
parity. The production integration checks real surface handoff; pixel readback
remains in the separate offscreen test. The correction in #15 obtains the worker-selected input after successful
acquisition. `Renderer::acquire_frame` returns an `AcquiredRenderFrame` holding
an exclusive renderer borrow; app prepares its lease, then consumes the guard
with `render`. Surface recovery and UI cleanup stay in shell; app owns fallback
and submission accounting, worker owns clock/PTS eligibility. Video preparation
is excluded from shell timing to avoid counting it twice.

The additional ignored test
`frame_prepare::shared_frame_materialization::cadence_tests::cadence_surface_tests::acquired_surface_preserves_video_ownership_and_recovers_after_drop_and_error`
uses a real X11/Vulkan shell with a fake two-frame provider. It verifies clear-only
presentation, acquired-frame abandonment, typed video failure, recovery through
existing resize/reconfigure, subsequent video presentation and exactly-once shared
lease release after GPU completion. Run it alone using the same `cargo test`
`--ignored --exact` pattern. WGPU 29 Vulkan discard is a no-op, so abandonment or
fatal video failure still needs the existing surface lifecycle recovery; the guard
does not introduce implicit reconfiguration or change the normal error policy.
