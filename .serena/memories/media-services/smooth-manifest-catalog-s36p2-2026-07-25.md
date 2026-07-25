# S36P2 — Smooth VOD manifest preparation and C3 catalog (2026-07-25)

## Ownership and dependency boundary
- New guarded first-party `web-media-smooth` consumes one neutral S36P1 `TransportOpenRequest`; it has no `service-ytdlp`, app, player, demux, concrete HTTP or UI dependency.
- P2 owns one bounded S31 manifest fetch, cancellable S36D parse, strict base VOD profile validation, exact zero-start/root-bound evidence, F2 mapping/init readiness for every advertised quality, and one neutral C3 `VideoAndAudio` catalog plus provider default selection.
- All budgets are caller-supplied in `SmoothPreparationPolicy`: adaptive limits/retry, XML budgets, manifest limits, per-init limits, aggregate init bytes and catalog limit. No production defaults or hidden constants exist.

## Fetch, security and retained seed
- Preparation requires neutral VOD/Muxed transport shape with no range override. It performs exactly one full Manifest fetch with `BypassScopedQuery`; no fragment request occurs.
- `AdaptiveFetchedResource::final_target()` after redirects is the sole authoritative fragment base and is retained privately with the `AdaptiveHttpContext`, `Arc<SmoothManifest>` and per-quality runtime rows. Raw original/final targets, XML, headers, cookies, codec bytes and custom-attribute values are absent from public Debug/errors.
- The public prepared result exposes only immutable catalog, provider default selection, source generation and exact aligned span. The runtime seed remains crate-private for P3.

## Truthful profile and catalog
- The admitted S00 base profile requires exactly one nonempty video stream and one nonempty audio stream. Both start at exact zero; video ends exactly at authoritative root duration, while audio may end naturally earlier but never later. `SmoothAlignedSpan` preserves root/video/audio ends and exact `min(video,audio)` common end without tolerance, silence synthesis or rescaling; differing clocks and chunk boundaries are valid.
- Every declared quality must map through F2 and build a bounded owned initialization segment before publication. One malformed/unsupported row or any per-row/aggregate budget failure rejects the whole profile; partial filtered catalogs are forbidden.
- Runtime rows retain declared Smooth track selection plus built init and C3 exact identity; borrowed mapped tracks are never stored.
- Refresh-stable semantic keys are versioned length-framed SHA-256 records with exact formats `ss-v1-v-<64hex>` and `ss-v1-a-<64hex>`. They include admitted codec/descriptor fields and sorted custom attributes but exclude XML order, declared quality index, stream ordinal, clock, URL/template, target and codec-private bytes. Length framing is fallible/typed, never saturated.
- Video order is height/width/bitrate descending then key; audio is bitrate/rate/channels descending then key. Default video reuses C3 `PreferredHeightPolicy`; default audio is the first best row. Catalog generation is explicit caller-owned state, never derived from source generation or manifest bytes.

## Cancellation and verification
- Cancellation is collapsed across fetch/parse/mapping/init and fenced after row sorting, before catalog construction and immediately before publication. A complete but canceled catalog is discarded.
- Accepted checks: 9 crate tests including local redirect/effective-base/secret stripping, all-quality init, alignment clocks/mismatches, extra stream, deterministic/index-order-invariant keys, aggregate budget and final cancellation; 112 neighbor tests; strict Clippy/rustdoc; Rust 1.92 all-target compile; dependency/role/coverage/format guardrails; exact normal dependency tree; fmt/diff/Serena diagnostics.
- ISO patch inventory and dependency integration gate include `web-media-smooth` (audio 77, smooth-fmp4 7, symphonia-demux 172, video-vaapi 137, web-media-smooth 9).

## Downstream boundary
- P3 now consumes the private seed without reparsing: it validates exact C3 rows, reuses retained init, plans F2 fragment paths, resolves them against the retained effective target, reconstructs boundedly, and exposes ordinary video plus window-aware audio ordered sources. No catalog/default reinterpretation belongs downstream. See `mem:media-services/smooth-fragment-sources-s36p3-2026-07-25`, `mem:media-services/smooth-request-projection-s36p1-2026-07-25` and `mem:media-services/smooth-streaming-fmp4-mapper-s36f2-2026-07-25`.