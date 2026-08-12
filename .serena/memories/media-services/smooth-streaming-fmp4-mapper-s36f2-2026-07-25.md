# S36F2 — pure Smooth manifest-to-fMP4 mapper (2026-07-25)

## Ownership and dependencies
- New first-party `smooth-streaming-fmp4` is the pure adapter between sealed `smooth-streaming-manifest-core` values and the opt-in reconstruction API in local `symphonia-format-isomp4-patch`.
- Normal dependencies are exactly `smooth-streaming-manifest-core`, `symphonia-format-isomp4`, and `thiserror`; dependency/role/coverage guardrails forbid HTTP, source, provider, demux, player, decoder, app, seek, live/DVR and DRM ownership.
- The crate selects a validated manifest stream/quality, maps codec/init fields, renders a safe relative fragment path through the manifest template owner, seals fragment identity/window/reconstruction intent, and classifies reconstructed coded coverage. It never accepts an arbitrary or absolute path; the relative path has no Display, a length-only Debug, and an intent-named transport accessor.

## Mapping and initialization
- Each independent audio/video single-track resource uses ISO track ID 1. Track timescale is the selected Smooth stream clock; fragment base DTS is the exact manifest fragment start; sample defaults are absent. Video uses proven-RAP intent, audio has no fake RAP requirement.
- H.264 mapping splits only canonical four-byte start-code codec private data, identifies exactly one SPS NAL type 7 and one PPS type 8 regardless of order, strips only delimiters, and passes selected quality dimensions into F1 init validation. Media payload remains unchanged four-byte length-prefixed NAL data.
- AAC passes the exact manifest-owned ASC bytes, including explicit and derived-from-quality proofs, plus selected quality sample rate/channels into F1 AAC-LC validation. The adapter does not rederive or rewrite ASC.
- Relative fragment paths are rendered only through the validated manifest template context using bitrate, exact fragment start ticks and selected quality custom attributes.

## ManifestWindow versus coded coverage
- `SmoothManifestWindow` and `FragmentCodedCoverage` remain separate values in the same stream clock. Classification compares raw `u64` ticks only: start mismatch first, then exact equal ends, coded overhang or coded underrun. There is no float, rescale, nanosecond conversion, epsilon, tolerance or retiming.
- Admission policy is strict: exact video/audio is `Admitted`; start mismatch rejects; video overhang rejects. Audio overhang returns a pending bounded presentation window with `ClipOverhang`; a positive underrun returns a pending bounded presentation window with `SubsampleUnderrun` only when it is strictly below one decoded PCM frame (`missing_ticks * sample_rate < timescale`). A one-frame-or-larger underrun remains incompatible. Bytes, exact manifest window, coded coverage, clock/sample-rate evidence and sealed identity remain unchanged.
- F2 intentionally accepts no capability bool/token/trait. Pending audio is not playable/admitted. A later decoder/player owner must prove and execute exact PCM presentation-window clipping before admission, including the one-tick case.
- Internal media state is enum-shaped `Video | Audio(format)`; no optional audio format or panic-backed cross-field invariant remains.

## Exact evidence and verification
- Tests reuse the single checked-in Smooth/PIFF corpus without copying media binaries. Three video fragments are admitted exact: `[0,40_000_000)`, `[0,40_000_000)`, `[40_000_000,80_000_000)`. Audio fragment one remains pending for exact overhang `426_666` ticks (`39_680_000` manifest end vs `40_106_666` coded end); audio fragment two remains pending for exact overhang `1` tick (`79_573_333` vs `79_573_334`).
- Focused tests also prove exact rendered paths/windows, H.264 401k/1501k init, AAC 64k ASC `11 90` at 48 kHz stereo, derived AAC ASC, reversed SPS/PPS order, track ID/timescale fields, determinism, typed budgets/cancellation/redaction, underrun/video-overhang and mutated-`tfdt` start mismatch.
- Accepted verification: 7 integration tests, strict all-target Clippy, Rust 1.92 check, manifest-core suite, patch suite, dependent matrix, dependency/refactor/format guardrails, fmt, diff and Serena diagnostics.

## Completed downstream boundary
- F3A/F3B now transport explicit packet windows and clip decoded PCM before tempo/output while legacy formats remain explicitly unbounded; P3–P6 admit the proven pending audio into the production Smooth runtime. See `mem:media-services/presentation-window-transport-s36f3a-2026-07-25`, `mem:player-core/exact-pcm-presentation-window-s36f3b-2026-07-25` and `mem:media-services/smooth-vod-runtime-s36p4-p6-2026-07-25`.