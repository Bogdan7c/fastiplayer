# Benchmark policy

Product names and result labels were normalized to Fastiplayer after the rename. All measurements, dates, samples and original source/binary hashes still refer to the historical revisions recorded below. Original labels remain available in Git history; these results are not measurements of the renamed build.

Fastiplayer publishes measurements with their scope, environment, and limitations. Performance is a project priority; published claims must describe the operation actually measured. A faster source-opening fixture does not establish lower video playback CPU use, battery savings, or superiority over another player.

## Existing N15 ingress experiment

The existing [N15 methodology and acceptance report](../native-web-ingress-n15-acceptance.md#performance-30-cold--30-warm) is in Russian. Its [performance aggregates](../native-web-ingress-n15-performance.json) and [acceptance outcomes](../native-web-ingress-n15-acceptance.json) record the measured data. This English guide makes the scope and reproducibility limits explicit.

- Recorded on 2026-09-02, at code revision `c330ba74`. The acceptance report records binary hashes and distinguishes the later G3 build. Historical revisions may need provenance mapping during the public-history cutover.
- Five cohorts each contain 30 successful repetitions: legacy cold, legacy warm, native cold Ogg, native warm Ogg lifecycle, and native warm HLS lifecycle.
- Latencies are milliseconds, RSS is KiB, and CPU occupancy is a percentage of one logical core. p95 uses nearest rank, `ceil(0.95 × N)`; medians and p95 are separate statistics.
- The matched cold comparison uses legacy extractor-fixture opening versus native Ogg opening. Native catalog median/p95 is 4.3205/4.403 ms; first-consumer median/p95 is 5.3235/5.559 ms. The README rounds those values to three decimal places.
- Warm Ogg measures open, forward/back seek and refresh; warm HLS adds switching. Do not relabel their consumer times as window startup or video first-frame times.
- Legacy payload bytes describe a small extractor metadata fixture, while native bytes describe media transport. They are not comparable throughput measurements. Warm legacy counters accumulate reopen work and must not be divided by native lifecycle counters to produce a speed ratio.
- CPU occupancy increases over the shorter native wall interval even though reported CPU time decreases. Do not equate CPU occupancy, CPU time, and power consumption; do not compute a new combined p95 by adding separate percentile values.
- N00 has no thirty-run latency distribution. Only its recorded structural/process counts can be compared: total cold extractor spawns went from 11 to 2, and direct-row spawns from 9 to 0.

**Reproducibility limit:** the public tree contains sanitized aggregates, not original per-run samples. N15 raw artifacts were retained locally under ignored `target/native-web-ingress/n15/`, and its exact owner-controlled acceptance corpus is not distributed. The linked report includes the recorded commands and corpus checksum, but this is not a self-contained public benchmark kit. Do not invent raw samples, confidence intervals, a runnable N15 performance command, or broader results to fill that gap. Functional consumer tests can be rerun using the commands in the report; they are not substitutes for the original timing cohorts.

The [ThinkPad T480s report](thinkpad-t480s.md) is a separate completed S08 experiment with raw playback measurements, a real runtime capture and explicit comparison limits. N15 is not a VLC comparison.

## Rules for new benchmark reports

1. **Identify the measured system.** Record source revision, release binary SHA-256, build profile/flags, toolchain, CPU/GPU/RAM, OS/kernel, graphics session, driver/Mesa/libva/Vulkan versions, display resolution/refresh, audio configuration, and power mode. Do not publish hostname, username, home paths, serial numbers, device UUIDs or machine IDs.
2. **Identify the media legally and reproducibly.** Use owner-controlled, appropriately licensed, or synthetic fixtures. Include redistribution/license status, codec/profile, resolution, frame rate, bit depth, duration, and checksums. Provide lawful acquisition or generation instructions if the file cannot be redistributed. Remove private URLs and credentials.
3. **Prove playback before timing it.** Verify source → demux → decode → real render submission/presentation → audio, or document an explicitly muted scenario. Record the actual decoder and transfer path. A capability query, successful open, or decoded frame alone is insufficient.
4. **Define every metric.** Specify its start/end events, units, collector, sampling interval, process/child-process scope, and warm/cold cache policy. Distinguish audio first consumer, video first frame, GPU submission, and physical-window presentation. Missing or incomparable metrics stay unavailable.
5. **Preserve the observations.** Publish sanitized per-run machine-readable results, exact commands, sample counts, failed/excluded attempts, and calculation rules. Do not silently drop slow runs or treat a profile exclusion as successful playback.
6. **Keep cohorts separate.** Compare the same operation, fixture, profile, environment, and instrumentation. Report medians, p95 where justified, and observed ranges. Do not pool warm/cold samples, revisions, codecs, or machines.
7. **Describe limits next to the claim.** State hardware availability, software fallback, network variability, missing instrumentation, and untested cases. Scope headline percentages to the actual experiment.

## S08 sampling and VLC control

Use release builds and at least three warm-up runs. For steady-state playback, collect **five 60-second measurements per scenario**. Measure CPU, RSS, and dropped/late frames only when the instrumentation is reliable. Startup/first-frame/seek measurements use **30 runs only if an existing harness measures the exact operation reproducibly**; otherwise mark the metric not measured and explain why. Record cache conditions instead of assuming a warm-up makes every subsequent operation warm.

A VLC control must use the same file, display/session, confirmed hardware decoding, audio/mute policy, warm-up count, five 60-second windows, and CPU/RSS collector/process scope. Compare dropped frames only when both counters mean the same thing. Record VLC version and settings, including processing that could alter workload.

If equivalent conditions cannot be established, **omit the VLC comparison table** and publish the Fastiplayer hardware baseline with the reason. There is no requirement to produce a favorable outcome or a comparison at all. Do not use an unqualified “faster than VLC” headline.

## Screenshots and publication

A screenshot must show the real application playing the documented fixture. Design concepts are never runtime evidence. Check for personal paths, notifications, URLs, and credentials before publishing. S08's real main-window capture is preserved in the [tagged asset directory](https://github.com/Bogdan7c/fastiplayer/tree/v0.1.0-alpha.1/docs/assets); its source and movie attribution are recorded in the T480s report.

Update the landing page only after reviewing the report, raw results, and playback evidence together. Keep N15 and T480s provenance separate. Benchmark-policy changes do not retrospectively make the older N15 aggregates satisfy newer raw-data requirements.
