# N15 native web ingress acceptance — 2026-09-02

Связано с `mem:core`, `mem:testing/web-media-playlist-acceptance-2026-08-04`, `mem:testing/playback-smoke` и `mem:testing/media-fixtures`.

## Tracked evidence и commits

- Code fixes: `c330ba74` (`fix native public media acceptance paths`).
- Sanitized docs/evidence: `d3432f85`.
- `docs/native-web-ingress-n15-acceptance.md`
- `docs/native-web-ingress-n15-acceptance.json`
- `docs/native-web-ingress-n15-performance.json`
- Roadmap остановлен перед G3.
- Exact XSPF SHA-256: `1daa973aa0f16a3be93e588dd3c83a8432b2917a5b525a05eb278776bb9c6435`, 13 rows, replacement count 0.

## Public outcome

Final release SHA-256 `5cfc3c08979a07573a63d1b8d48637a40afaec250d883213bf9e1c1506a38fed`.

- 11 available rows reached full startup presentation/audio gate.
- `row04` is `PROFILE_EXCLUDED`: exact live HLS requires avc3 + HE-AAC + TTML outside accepted native playback profile.
- `row12` is `PROFILE_EXCLUDED`: exact DASH source carries picture/sample-aspect evidence not representable by current square-pixel display contract.
- No final PLAYER_FAILURE, SOURCE_DRIFT or SOURCE_UNAVAILABLE.
- Process spy exact cold/restart set is `{row00,row08}`, one spawn per listed run. All 11 direct rows use `yt_dlp.enabled=false` and spawn zero extractor processes.
- Auto representative direct/HLS/DASH/live PASS.
- Real hardware preflight PASS on AMD Radeon 780M / Mesa 26.2.1 / renderD128 / VAProfileAV1Profile0 VLD.
- Hardware public HLS/DASH/live PASS. Local VP9 SDR auto, AV1 SDR hardware, AV1 HDR P010 hardware PASS.
- HDR runtime selected active `BT.2020 PQ limited -> SDR BT.709 bt2446-c explicit-shader-oetf` path and submitted 1192 frames. WGPU submit/readback integration and 13 BT.2446-C reference/shader tests PASS.

## Root-cause fixes and boundaries

1. DASH MPD parser:
   - consumes only explicit known non-playback text adaptations (MP4 wvtt/stpp or text/vtt);
   - DRM remains manifest-wide fail-closed;
   - exact XSI schemaLocation allowlist; arbitrary namespaced attrs remain rejected;
   - subsegmentAlignment is boolean-validated;
   - an unsupported SAR representation is isolated only after its subtree is consumed; playable siblings remain;
   - adaptation-wide PAR still fails closed if no representation can preserve geometry.
2. DASH catalog proof:
   - zero-based SegmentBase Initialization range yields optional `catalog_probe_content_length`;
   - `DashComponentOpenIntent::{Playback,CatalogProof}` keeps bounded prefix exclusive to catalog proof;
   - `AdaptiveRangeSourceConfig::with_exposed_content_length` separates consumer-visible prefix from validated physical representation;
   - normal playback still sees full content length and full seek lifecycle.
3. Startup readiness:
   - `StartupPreparedConsumerProof` carries authoritative audio and video topology;
   - audio-only beginning completes on actual audio resume without inventing a video surface;
   - video and restore gates preserve prior surface/seek requirements.
4. HLS alternate audio:
   - multiple renditions select exactly one provider DEFAULT; absent/ambiguous default remains fail-closed;
   - catalog alignment compares cumulative presentation duration plus discontinuity layout, not exact per-segment EXTINF values, because AAC access-unit boundaries may oscillate around video boundaries.
5. Terminal preparation logs emit only already-sanitized error strings.

## Verification

- `cargo +1.96.0 test -p dash-mpd-core --locked` PASS.
- `cargo +1.96.0 test -p web-media-adaptive -p web-media-dash --locked` PASS.
- `cargo +1.96.0 test -p web-media-hls --locked` PASS.
- Focused app HLS/DASH consumers and startup readiness PASS.
- `cargo +1.96.0 test -p app-egui n14b_lifecycle_ --locked`: 17/17 PASS, covering seek/switch/recovery/reopen/restart/stale fences.
- Strict Clippy affected packages PASS.
- `cargo +1.96.0 check --workspace --all-targets --all-features --locked` PASS.
- fmt, diff check, release build PASS.
- `scripts/playback-smoke.sh --mode hardware-only ...` PASS.
- G3 intentionally NOT RUN.

## Performance

30 repetitions per cohort, nearest-rank p95.

Matched cold native vs legacy:
- catalog median/p95 improves 85.35%/85.60%;
- first consumer 82.11%/81.99%;
- wall 72.99%/72.89%;
- combined CPU time 26.64%/30.95%;
- RSS 6.91%/7.05%.
N00 cold extractor spawns 11 -> N15 2; direct-row spawns 9 -> 0.
Raw per-run evidence remains ignored under `target/native-web-ingress/n15/`; only sanitized aggregates are tracked.
