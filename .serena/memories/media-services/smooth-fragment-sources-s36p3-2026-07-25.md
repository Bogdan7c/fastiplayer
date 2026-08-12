# S36P3 — selected finite Smooth fragment sources (2026-07-25)

## Ownership and public boundary
- `web-media-smooth::SmoothPreparedCatalog::into_selected_fragment_sources` consumes the retained P2 seed plus an exact C3 `ComponentVariantSelection` and caller-owned `SmoothFragmentSourcePolicy`.
- Selection is revalidated through the immutable catalog, requires `VideoAndAudio`, and runtime rows are matched by exact descriptor identity rather than catalog position. Missing/duplicate rows and remap drift are typed provider invariant failures.
- The result retains canonical catalog/selection, source generation and `SmoothAlignedSpan`, and exposes an ordinary finite `SmoothVideoFragmentSource` plus `SmoothAudioFragmentSource` implementing the presentation-window-aware ordered source contract.
- Construction remaps the selected rows only as proof and performs zero fragment HTTP. First pull publishes retained init at sequence 0 with `Continuous`; media sequence is fragment index + 1; repeated EOS performs no HTTP.

## Cursor transaction and reconstruction
- Both axes reuse the same retained S31 session/cookie/cancellation/generation context; the video context is cloned and audio receives the original. No HTTP client or cookie jar is rebuilt.
- Each pull remaps the immutable manifest selection, plans a sealed F2 relative path, resolves it against the final post-redirect manifest target, performs one bounded full MediaSegment fetch with `BypassScopedQuery`, and calls the generic F1/F2 reconstruction boundary.
- Cursor index advances only after mapping, path resolution, fetch, reconstruction, exact audio-window materialization and final cancellation fences all succeed. Cancellation never advances or latches; deterministic failure latches one bounded redacted failure and neither skips nor refetches.
- Video accepts only F2 `Admitted` and publishes ordinary ordered media. Audio maps `Admitted` to `PacketPresentationWindow::Unbounded`; either pending adjustment (`ClipOverhang` or strictly sub-sample `SubsampleUnderrun`) keeps reconstructed bytes unchanged and publishes the same exact bounded manifest window using F2's authoritative reconstructed track ID, checked i64 tick conversion and native timebase. F3A/F3B own packet transport and decoded-PCM clipping; a full-frame-or-larger underrun remains terminal.

## Secret forwarding boundary
- `web-media-adaptive` now has additive typed `AdaptiveResourceSecretForwarding::{ForwardScoped, Suppress}`. Existing `AdaptiveResourceFetchRequest::full/range` defaults remain `ForwardScoped`, so HLS/DASH behavior is unchanged.
- `AdaptiveHttpContext::resource_secret_forwarding_for` derives a retained intent for an effective target from the existing S21T secret scope. P2 stores it once for the final manifest base; P3 applies it to both source cursors.
- `Suppress` never asks the secret context for scoped headers, Cookie/query material and therefore permits cross-origin fragment continuation without credentials. The intent survives retry; redirect authorization can only remove forwarding, never restore it. There is no secret-bearing fallback retry.

## Truthful finite span
- Root and video end must be exact-equal. Audio may naturally end earlier but never later; no silence, tolerance or fake padding is introduced. `SmoothAlignedSpan` exposes root/video/audio/common ends independently.
- Canonical PIFF evidence at 10,000,000 ticks/s: root/video = 7,340,000,000; audio/common = 7,339,363,333.

## Focused proof and gates
- Hermetic/canonical tests cover stale/wrong-layout selection, descriptor-to-row mapping, zero-HTTP construction/init, effective-base paths, request order and sequence, low/high video, exact bounded canonical audio windows, corpus-derived exact audio `Unbounded`, unchanged pending bytes, malformed/tfdt mismatch/underrun/overhang, body/F1 inspection/F1 write limits, cancellation without advance, terminal latch, repeated EOS, redaction, cross-origin Authorization/Cookie suppression and same-origin forwarding.
- Accepted after root review: `web-media-smooth` 25 tests, `web-media-adaptive` 24 tests, canonical F2/manifest and HLS/DASH regressions, strict Clippy, Rust 1.96 check, rustdoc, fmt, diff check, guardrails and dependency gate. Serena diagnostics are clean.

## Completed downstream boundary
- P4/P5/P6 are complete: injected S28A/F3A adapters, stable A/V composite, worker-owned readiness, transactional receipted VOD seek and app two-phase C3 composition are documented in `mem:media-services/smooth-vod-runtime-s36p4-p6-2026-07-25`.

Related: `mem:media-services/adaptive-transport-s31-2026-07-23`, `mem:media-services/smooth-manifest-catalog-s36p2-2026-07-25`, `mem:media-services/smooth-streaming-fmp4-mapper-s36f2-2026-07-25`, `mem:media-services/presentation-window-transport-s36f3a-2026-07-25`, `mem:player-core/exact-pcm-presentation-window-s36f3b-2026-07-25`.