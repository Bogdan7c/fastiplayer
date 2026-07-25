# S36F1B — bounded generic fragmented MP4 reconstruction (2026-07-25)

## Ownership and public boundary
- The existing local `symphonia-format-isomp4-patch` remains the only ISO-BMFF owner. Its opt-in `fragment_reconstruction` boundary now exports two independent operations: `build_fragmented_initialization_segment(FragmentInitializationRequest)` and `reconstruct_media_fragment(FragmentReconstructionRequest)`.
- This patch API is container-generic and does not depend on `smooth-streaming-manifest-core`, HTTP, provider/app/player state, ManifestWindow, VOD duration, seek or live/DVR policy. Existing `IsoMp4Reader`, registry and ordered/local/progressive/HLS/DASH paths are unchanged; no existing runtime calls the new API until later S36 composition cards.
- Inputs use typed track IDs, timescales, codec intents, media/RAP intent, sample defaults, mandatory inspection/write budgets and callback cancellation. Initialization and reconstruction errors are typed; reconstruction keeps `Inspection` and `Writing` causes distinct. Debug/Display never include codec or media payload bytes.

## Initialization segment
- The builder emits a deterministic separate single-track `ftyp + moov` with one `trak`, matching `mvex/trex`, self-contained data reference and empty classic sample tables. Durations are zero; `trex` description index is one and defaults are zero; no `mehd`, `sidx`, `edts`, `moof` or `mdat` is written.
- Supported codec configurations are narrow: H.264 `avc1` with exactly one validated Annex-B-free SPS/PPS converted to `avcC` with four-byte NAL lengths, and AAC-LC `mp4a.40.2` with exact two-byte validated ASC consistent with typed sample rate/channels. HE-AAC, unknown extensions, malformed parameter sets and unrepresentable fields fail closed.
- Planning uses checked box sizes, mandatory output/codec limits, one fallible exact allocation and cancellation fences before planning, after planning and before publication.

## Media fragment reconstruction
- F1A first boundedly inspects untrusted Smooth/PIFF `moof+mdat` and produces a private normalized sample plan with exact DTS, PTS, duration, CTO, flags, byte ranges and `FragmentCodedCoverage`. Manifest duration is intentionally not an inspector/writer input.
- The writer emits deterministic canonical `moof(mfhd, traf(tfhd, tfdt, trun)) + mdat`: exact sequence/track/base decode time, `default-base-is-moof`, `tfdt` v0/v1, one explicit run, checked data offset and sample durations/sizes. Video requires proven flags for every sample; audio does not invent RAP and preserves flags only when uniformly representable. CTO uses unsigned v0 when possible or signed v1 when required; unrepresentable mixed ranges fail typed. PIFF metadata is not copied.
- Sample payload order and bytes are preserved exactly, with one fallible exact allocation. Inspection and writer budgets remain separate. Cancellation is polled before/within inspection, while planning/writing sample tables, before payload copy and before publication.

## Exact evidence and verification
- Checked-in fixtures under `crates/symphonia-format-isomp4-patch/fixtures/smooth-piff` are the Unified Streaming Tears of Steel manifest plus two H.264 qualities/fragments and two AAC fragments; `PROVENANCE.md` records exact URLs, SHA-256, sizes and CC BY 3.0 source licensing.
- Generated H.264 401k/1501k and AAC 64k init segments open through production `IsoMp4Reader`. Canonical media round-trip reinspection preserves sequence, track, coded coverage, DTS/PTS/duration/CTO/flags and every payload.
- Production concat proof: H.264 init plus two fragments yields 192 exact packets; AAC init plus two fragments yields 188 + 187 packets and retains coded coverage ends `40_106_666` and `79_573_334` rather than retiming to manifest windows.
- Accepted suite after root review: patch crate 85 tests, strict all-target Clippy, Rust 1.96 downstream `demux-api`/`symphonia-demux`/`web-media-http`/`codec-core`/`audio`, dependency-patch integration gate (audio 77, symphonia-demux 163, video-vaapi 137), fmt, diff, guardrails and Serena diagnostics.

## Downstream invariant
- Later Smooth mapper policy must keep `ManifestWindow` distinct from generic `FragmentCodedCoverage`. The generic writer never clips, stretches or retimes samples. Audio overhang may only be admitted by a later Smooth/player boundary that can prove and enforce exact PCM presentation-window clipping; underrun remains incompatible. See `mem:media-services/smooth-streaming-vod-s36d-2026-07-24` and `mem:dependency-patches/core`.