# Native network fast-start and responsive seek (2026-09-04)

## Scope and observed root causes

Session optimized only native network formats that already had working hermetic playback paths. Broken public/provider paths were not expanded.

- HLS catalog/content proof used `OrderedSegments`, which forced a complete first MPEG-TS segment download for every sibling although TS discovery only needs a bounded PSI/container prefix. This made x36xhzz cold open approximately 18.3 s.
- HLS drag preview could pin an old observed packet anchor; the final worker then treated that preview as exact and decoded a large stale interval instead of reopening near the manifest target.
- DASH catalog proved every representation sequentially and separate video/audio initialization/seek preparation was serialized.
- DASH `SegmentBase` used one HTTP Range request per small Symphonia read. A real +30 s seek issued about 99 packet-sized Range requests and took approximately 13.07 s, while the same MP4 locally sought in about 98 ms. The root cause was the network byte-source contract, not decoder or RAP search.
- Smooth video/audio component readiness was serialized.
- Global automatic quality picked the highest ranked playable rendition before runtime network evidence, increasing startup cost and stall risk.

## Architecture and invariants

### Shared adaptive Range transport

- `web-media-adaptive::AdaptiveRangeByteSource` owns bounded read-ahead pages. Its config explicitly supplies maximum throughput page bytes, latency-first page bytes, maximum cached pages, and query application.
- DASH production uses a 64 KiB latency-first page, a 1 MiB throughput page, and two retained pages per component. Page starts are aligned; two pages let init/index and the current media window coexist.
- A far cursor discontinuity arms one latency-first request. Sequential misses switch to the throughput page. Cached reads and nearby seeks perform no HTTP request.
- Blocking adaptive fetch now participates in the existing completed-only VOD LRU using the same exact key as streaming fetch: target, Range, maximum body bound, purpose, query application, and secret-forwarding policy.
- Only fully successful buffered media/init responses are inserted. Cancellation, truncation, transient partial bodies, live resources, manifests, and in-flight responses are never replayed. Fresh transactional Range sources therefore reuse completed probe/pages without weakening rollback or generation fencing.
- Range identity, physical total length, validators, redirect/secret policy, cancellation, and memory bounds remain checked by their owning transport boundaries.

### HLS

- HLS catalog/container proof calls one shared `open_epoch_probe`. MPEG-TS is opened as a pull-stream, so registry sniffing can stop after a bounded prefix; fMP4 gets the finite segmented fallback only for `NoMatch` or unsupported input, never for transport/parse failures.
- Native VOD initial/final seek uses containing-segment `DecodeFromOrBeforeTarget`; decoder preroll enforces the presentation floor. No pre-target frames may be presented.
- `ProgressiveSeekController::manifest_reanchored` is an explicit provider boundary. Normal progressive seeks still require exact preview equality. HLS alone may replace a stale `DecodePointBefore` preview with a fresh manifest-derived anchor no later than the same requested target.
- Muxed and separate A/V HLS final seek both select the receipted manifest candidate and commit only the completed generation.

### DASH and Smooth

- DASH catalog has a batch proof boundary. Injected ports retain sequential default behavior; production executes a caller-bounded worker set (app limit 16), restores request order, isolates lane failures, and never publishes an incomplete proof batch.
- DASH separate video/audio initial open and receipted seek use scoped parallel preparation. Both workers are always joined and composition/active mutation happens exactly once only after both succeed.
- Smooth video/audio ISO-BMFF readiness now uses the same scoped parallel pattern. A panic or error is converted at the provider boundary and no partial component pair is published.

### Automatic quality / UX policy

- Global Automatic/BestPlayable starts at preferred 720p (the default viewport scale) for latency-first playback.
- Only automatic mode adapts, one catalog-owned adjacent resolution step at a time. The controller never reads catalog storage or constructs fake selections.
- Manual 1080p/1440p/2160p remains strict exact behavior and never auto-downshifts.
- Automatic upshift requires 30 s stable playback plus runway. New audio underrun can downshift immediately; continuous buffering downshifts after 750 ms. Failed height retry is 120 s; anti-flap decision hold is 5 s after an actual reinstall, not on the first runtime.
- Automatic same-item switch preserves playback position/state and installed exact selection but never persists the adaptive target as a manual item preference.

## Functional evidence and locations

- Adaptive Range tests cover invalid latency/max config, bounded aligned read-ahead, nearby seek without RTT, far seek, total/validator fencing, and a fresh transactional source replaying completed probe/page without network: `crates/web-media-adaptive/src/tests/range_source.rs`.
- HLS 10 MiB slow-tail catalog test proves discovery returns before segment EOF and then reaches the selected demux runtime: `crates/web-media-hls/tests/catalog_runtime.rs`.
- HLS receipted seek tests cover late seek, containing-segment decode-forward, cancellation, A/V atomic commit, and actual demux packets: `crates/web-media-hls/tests/receipted_manifest_seek.rs`.
- DASH slow-init multi-lane test proves four overlapping initialization requests and reads a selected runtime packet: `crates/web-media-dash/src/tests.rs`.
- DASH and Smooth component tests prove maximum active preparations is two and runtime A/V/receipted seek reaches both component results.
- App lifecycle/controller tests cover automatic startup height, adjacent catalog target, stable upshift, underrun/buffering downshift, failed-height retry, initial-vs-reinstalled hold, persistence isolation, and position/play-state preservation.

## Real acceptance evidence on 2026-09-04

Release binary, fresh config roots:

- HLS x36xhzz cold accepted-to-initial-anchor: approximately 1.61 s in final run (earlier post-fix run approximately 1.47 s), versus approximately 18.3 s before.
- HLS x36xhzz +120 s accurate seek: 414 ms receipt, 448 ms first correct presented frame, 450 ms commit, 0 presented pre-target frames; receipt-to-present was 34 ms. An earlier network run was 257 ms.
- DASH angel-one cold open improved from approximately 33.56 s to approximately 2.05 s accepted-to-component-ready in the final two-page run (catalog activity begins before component-ready).
- DASH +30 s seek improved from approximately 13.07 s to 1.50 s first presented frame. Receipt was 1.467 s and receipt-to-present only 33 ms with 0 pre-target frames, proving the remaining time was the single remote target Range request/CDN latency rather than packet-by-packet reads or decoder work.
- Public Smooth fixture returned no playable tracks during the final external run; do not claim it as a public playback pass. Hermetic production-shaped Smooth A/V and receipted-seek functional tests pass.
- Public HDS fixture currently spends about 13 s and yields no tracks. It belongs to the user's explicitly excluded broken-format scope and was not broadened in this session.

## Verification

Final/near-final gates:
- `cargo fmt --all -- --check`: pass.
- focused provider/demux tests: demux-api 72, web-media-adaptive 65, DASH 43 + integration suites, HLS 69 + integration suites, Smooth 40: pass.
- `cargo test -p app-egui`: final 1094/1094 pass.
- strict targeted Clippy with `-D warnings`: pass.
- `cargo build --release -p app-egui`: pass on the final tree.
- `scripts/ci-checks.sh tests`: full workspace unit/integration/doc-test pass on the exact final tree after self-review.
- Serena diagnostics for all modified owner files: empty.
- `git diff --check`: pass.

The generic media-regression/progressive scripts require explicit fixture selections and were not counted as passes when they reported NOT RUN.
