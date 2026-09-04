# ThinkPad T480s playback baseline

**Status: NOT RUN — S08 placeholder.** No T480s performance results or runtime screenshot have been collected for this report. Existing [N15 measurements](README.md#existing-n15-ingress-experiment) describe a separate ingress experiment and must not be copied into this baseline.

<!-- S08 owns this report and docs/assets/rustiplayer-t480s-main.png.
     Fill only from verified runtime observations on the actual T480s.
     Never substitute design concepts, another machine, or invented metrics. -->

## Environment and provenance

Pending: exact source revision from the private green S07 checkout; release binary SHA-256; build command/flags; Rust version; CPU; integrated GPU; RAM; OS/kernel; Mesa/libva/Vulkan versions; graphics session and display resolution/refresh; power mode; audio route; and VLC version/settings if a comparison is feasible.

Confirm the machine model and actual VA-API decoding path. Do not include hostname, username, personal filesystem paths, serial number, machine ID, or device UUIDs.

## Scenarios

| Scenario | Status | Evidence required |
| --- | --- | --- |
| H.264 1080p60 | NOT RUN | Actual decoder/profile, presentation and audio proof |
| VP9 or HEVC 4K60 | NOT RUN | Choose one variant actually supported by this T480s; record unsupported hardware honestly |
| Optional subtitle/HDR case | NOT RUN / eligibility not assessed | Include only if implemented, working, and relevant; HDR → SDR is distinct from HDR display output |

Use only owner-controlled, appropriately licensed, or synthetic media. Add fixture descriptions, license/acquisition or generation instructions, checksums, and exact playback commands. An unsupported hardware case must not be replaced by software decode under the same label.

## Playback and runtime screenshot

Pending evidence: source open → demux → decode → real render submission/presentation → audio output, or a documented muted scenario. Record the actual codec/backend and frame transfer path.

The main runtime capture will be stored at `docs/assets/rustiplayer-t480s-main.png`. A second telemetry capture is optional. Check both for personal information. The README screenshot slot stays a placeholder until the real capture is available; `docs/design/` images cannot fill it.

## Measurements and raw results

Follow the [benchmark policy](README.md): at least three warm-ups and five 60-second steady-state measurements per scenario. Record CPU, RSS, and dropped/late frames only where trustworthy. Collect 30 startup/first-frame/seek observations only if the existing harness measures those operations reproducibly.

Pending: collector and event definitions, cache/power conditions, exact commands, sanitized per-run JSON artifact links, failed/excluded attempts, medians/p95 where justified, observed ranges, and artifact checksums. No zero-valued or illustrative result table is supplied: missing evidence is not a measurement.

## VLC control

**NOT RUN.** First establish equivalent fixture, display, confirmed hardware decode, audio policy, sampling and process scope. Use three warm-ups and five 60-second measurements. If equivalence cannot be established, omit the comparison table and document the reason; the Rustiplayer-only baseline is still useful.

## Limitations and S08 completion checklist

- [ ] Confirm T480s environment and supported hardware paths.
- [ ] Record fixture provenance and successful presentation/audio evidence.
- [ ] Capture the actual runtime window and remove personal information.
- [ ] Publish sanitized measurements, commands, hashes and statistical definitions.
- [ ] Clearly record unsupported/unmeasured scenarios and any VLC equivalence limit.
- [ ] Review claims against evidence; retain all material limitations.
- [ ] Commit the evidence and hand off its exact revision for S09 integration.

S09 will integrate the verified screenshot and scoped summary into the landing page. This placeholder does not complete hardware acceptance or public-launch readiness.
