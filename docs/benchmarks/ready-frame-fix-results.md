# Ready-frame acquisition fix: local qualification

Measured commit: `ca657f2c8ad5dff6ee203932ecfff8c7a2378f9e` (2026-09-09).
Related: [issue #15](https://github.com/Bogdan7c/fastiplayer/issues/15), [PR #16](https://github.com/Bogdan7c/fastiplayer/pull/16), tracking #10.

**Partial result: the proven Ready freshness defect is corrected; overall cadence qualification remains blocked.** This snapshot is named `current-ready-fix-v2`, not `quality-fixed`.

The app now acquires the surface before preparing worker-selected video input. Shell owns the acquired surface and UI cleanup; app owns materialization/fallback/submission accounting. Worker clock, PTS eligibility, GPU completion, decoder release and bounded capacities are unchanged. See [boundary and test documentation](frame-cadence-diagnostics.md).

## Resource measurements

The existing local protocol collected 60 valid attempts: five scenarios × two players × quiet/diagnostic × three repetitions. Player/mode order alternates; warmup is 12 seconds, measured windows are 30 seconds for synthetic and 15 seconds for real media, sampled at 1 Hz. Fullscreen is 1920×1080 at approximately 59.96 Hz, audio remains active but muted, with ordinary controls/animation and isolated configuration. Compilation/profiling did not overlap measurements. All ten scenario/mode comparisons stopped at the first planned look with WORSE CPU versus VLC; the planned 3/6/9 rule was unchanged.

CPU is process-tree percent (100% = one logical core). RSS/PSS are mean sampled process-tree sums in MiB; shared pages can be counted multiple times in RSS. The table shows quiet mode. Original is the preserved session-00 baseline, whose quality was already limited. Historical comparisons do not establish equivalent quality or eliminate host drift.

| Scenario | CPU original | CPU current | CPU VLC | RSS original → current | PSS original → current | Current vs VLC |
|---|---:|---:|---:|---:|---:|---|
| real-h264-1080p24 | 6.08% | 6.31% | 1.97% | 110.13 → 110.79 | 67.44 → 68.01 | WORSE |
| real-h264-1080p30 | 6.56% | 6.75% | 2.51% | 116.35 → 116.25 | 73.71 → 73.65 | WORSE |
| real-h264-1080p60 | 7.83% | 8.44% | 3.43% | 111.59 → 111.73 | 68.97 → 69.08 | WORSE |
| synthetic-h264-1080p60 | 7.93% | 8.43% | 3.25% | 111.26 → 111.68 | 68.72 → 68.97 | WORSE |
| synthetic-hevc-4k60 | 10.54% | 11.38% | 4.14% | 112.22 → 112.13 | 69.66 → 69.48 | WORSE |

All five quiet original/current CPU comparisons are INCONCLUSIVE under the conservative paired interval. All ten original/current RSS/PSS comparisons (means and sampled maxima) are INCONCLUSIVE. Small observed mean differences do not prove no growth; no resource-limit exception has been accepted. GPU retained allocation accounting remains unqualified (N/A), so this report makes no GPU-memory parity claim.

All individual current observations, including diagnostic mode and sampled memory maxima, are retained in [the sanitized CSV](ready-frame-fix-attempts.csv). The [structured comparisons](ready-frame-fix-comparisons.json) retain interval bounds and verdicts for both modes. A valid resource observation does not imply good cadence.

## Frame evidence

Pre-fix controlled reproduction handed off Ready 533.333 ms after 550 ms was published during acquisition. The final production-boundary matrix passed 18/18 attempts: 24/30/60 fps × publication before preparation/during acquisition × three. The gate checks Ready input identity and actual surface handoff. The real surface transaction test passed absent video, active fake input, typed video error, abandoned acquisition, existing reconfigure recovery, textured UI set/free ordering and exactly-once shared release after GPU completion. The offscreen consumer test checks actual changing pixels.

| Diagnostic source | Unique identities / surface handoffs, all three measured windows |
|---|---|
| real-h264-1080p24 | 360/900, 360/900, 360/900 |
| real-h264-1080p30 | 450/900, 450/900, 450/900 |
| real-h264-1080p60 | 900/900, 898/900, 900/900 |
| synthetic-h264-1080p60 | 1476/1800, 1800/1800, 1801/1801 |
| synthetic-hevc-4k60 | 1792/1800, 1789/1801, 1799/1800 |

24/30 fps naturally repeat frames on this approximately 60 Hz output. The 60 fps deficits, particularly 1476/1800, leave the overall anomaly open. Later successful windows do not erase this failure. These diagnostic resource logs alone do not attribute the deficit to Busy or prove physical scanout.

## Separate detailed traces

Six further quality-only traces (three synthetic H.264 60 fps, three real H.264 60 fps) were collected sequentially after the resource series, with detailed cadence tracing. They are excluded from CPU/RAM comparisons. Whole traces include warmup: synthetic handoffs were 2528/2527/2528 with Ready repeats 347/2/2 and Busy repeats 2/9/2; real handoffs were 1623 each with Ready repeats 6/2/2 and Busy repeats 0/0/1.

All six had zero prepared-input/handoff mismatches and zero instances of the specific old defect (newer same-generation publication during acquisition followed by older Ready handoff). In the anomalous synthetic trace, 339 of 347 Ready repeats had the same identity as the latest logged publication at preparation; eight saw a newer publication by the preparation event, outside the old acquisition-window predicate. This does not establish why publication lagged or identify every race inside preparation. The original resource-window deficit remains separately preserved. The bounded next investigation is the publication/selection timeline in this retained trace; changing worker scheduling or Busy policy requires a separate owner decision.

## Validation and limitations

- `bash scripts/pre-pr-checks.sh`: PASS on measured code, including workspace/feature checks, strict Clippy/rustdoc, policies and MSRV. Release build with `--locked`: PASS.
- Hosted CI on the measured commit: 21/21 SUCCESS after one rerun of a failed workspace job. The first run timed out in unchanged `local_file_open::tests::suspend_transfer_preserves_join_authority`; it passed locally and on the same-commit rerun. This attempt is retained, not treated as an initial clean CI run.
- Runtime lifecycle on final binary: 24/30/60 fps passed stable pause, resume, forward/backward and rapid seek/generation progress, EOF **with explicit fullscreen redraw assistance**. Idle MPRIS Play wake failed on both original and current binaries; ordinary idle wake is not certified and was not changed here.
- WGPU 29 Vulkan discarded surface recovery uses the existing reconfigure lifecycle; the new guard does not silently reconfigure. Fake lease tests do not establish the entire real decoder-to-GPU-release path.
- Serena rust-analyzer retains stale old-signature diagnostics after external edits; current Cargo compilation, full local checks and hosted CI pass. No code was changed to satisfy the stale snapshot.

The earlier measurement candidate was stopped after self-review restored UI texture cleanup to after submit and before poll/present. Its 20 completed valid observations and one interrupted invalid observation were preserved separately and excluded from this final 60-attempt dataset. An initial causal matrix also retained one Busy failure; it was not relabeled Ready or removed.

The draft remains open pending residual cadence diagnosis and owner acceptance. No merge, scheduler redesign, Busy-policy change or subsequent optimization stage is included.
