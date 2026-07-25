# S36D — secure Smooth Streaming VOD manifest boundary (2026-07-24)

## Ownership
- `smooth-streaming-manifest-core` is the pure, sealed MS-SSTR client-manifest owner. It depends normally only on `bounded-xml-reader` and `thiserror`; HTTP, yt-dlp, fMP4 reconstruction, demux, player, seek and app composition are outside the crate.
- Public callers can only obtain `SmoothManifest` through `parse_vod_client_manifest` or its callback-cancellable variant. Manifest/stream/quality/template/raw-timeline construction remains crate-private; public API is read-only getters, exact time/timeline iteration and typed fragment-path rendering.
- `BoundedXmlReader` is the only XML parser. Callers supply complete `XmlBudgets` and `SmoothManifestLimits`; namespaces/private extensions, unknown schema, DRM and malformed XML remain distinct typed failures. DTD/external entities are rejected by S04X and the exact `XmlReadError` remains the source.

## Approved profile and model
- VOD only, version 2.0 or 2.2, H.264/AVC video and AAC-LC audio. Root timescale defaults exactly to 10,000,000 when absent; stream clocks may inherit it. Live/lookahead/DVR, text/sparse/embedded/composite/trick streams, DRM, vendor/private constructs and other codecs fail closed.
- Every `StreamIndex` requires `Chunks`, `QualityLevels` and a safe relative `Url` template. Standard bitrate/start-time spelling variants and bounded `CustomAttributes` are typed; absolute/query/fragment/backslash/traversal and unknown placeholders are rejected.
- Every quality retains a typed declared `Index`, bitrate and bounded standard metadata. Duplicate indices are rejected. Equal bitrates are allowed only when `{CustomAttributes}` participates in rendering and the typed attribute sets differ; downstream never receives two qualities that render the same fragment predicate.
- H.264 codec-private data is bounded even hex with exactly one SPS and one PPS in four-byte-start-code layout and supported NAL length semantics. AAC requires `AudioTag=255`, AAC-LC object type, declared rate/channel consistency and either validated ASC or an explicit derived-from-fields configuration proof.
- Optional bounded stream Name/Language are preserved as typed identity metadata for later component-catalog semantic identity; raw parser constructors remain sealed.

## Timeline and bounds
- `c@r` uses MS-SSTR one-based total count: `r=2` means two fragments. It is valid only in v2.2. Lexical negative repeat is a dedicated rejection; positive input accepts the full `u64` domain before checked `usize`/budget admission.
- Timeline storage is O(runs), never O(expanded fragments). Missing first `t` means zero; later missing `t` uses previous end; missing `d` requires the next explicit start and exact divisibility. Zero duration/repeat, backward/overlap/discontinuity, count mismatch and arithmetic overflow are typed.
- `SmoothTime` compares rational clocks exactly with `u128` cross-products; it intentionally does not implement `Hash`. No float, nanosecond rounding, LCM or lossy rescale is used.
- Every stream interval lies within presentation duration and all published streams have a strictly nonempty exact common playback interval. Per-stream and aggregate budgets cover streams, qualities, raw timeline entries, normalized fragments, codec bytes and custom attributes transactionally.

## Verification and scope
- Focused corpus contains valid explicit H.264/AAC, v2.2 repeats, inferred timeline, differing A/V timescales/alignment, DRM, malformed XML/external entity, negative repeat, unsupported codec and malformed codec-private data. Cancellation is exercised before parsing and during XML/hex/timeline work.
- Final S36D verification: 32 unit + 14 external integration + 2 compile-fail doctests; strict all-target Clippy, fmt, dependency/guardrail checks, diff check and Serena diagnostics pass.
- S00 has no approved ISM live/DVR row. No S36L card/dependency/fixture exists; no S31L/S35S, refresh, DVR, `tfrf` or `tfxd` handling was added.
- S28A ISO-BMFF reuse is a downstream invariant: this crate neither creates nor parses MP4 boxes. fMP4 reconstruction and transport belong to later S36 cards.
