# S36P4–P6 — production Smooth VOD demux/seek/app composition (2026-07-25)

## Runtime ownership

- `web-media-smooth` consumes the exact P3 video/audio fragment sources through an injected `SmoothIsoBmffDemuxFactory`; named video/audio open requests transfer only source, cancellation and bounded sniff policy. The crate has no concrete Symphonia/app/player dependency.
- Blocking first-fragment fetch, ISO sniff/open and all parser reads run inside `ProgressiveDemuxer`; the player owner only polls readiness/events. Video uses the existing S28A ordered ISO-BMFF route, audio uses the F3A presentation-window adapter. Both axes must expose exactly one stable track before `CompositeAvDemuxer` publishes the stable public A/V snapshot and manifest duration.
- `symphonia-demux::PresentationWindowOrderedIsoMp4Demuxer::new_with_registry` is additive and accepts one composition-owned `Arc<DemuxRegistry>`. The adapter itself fixes required container identity to `iso-bmff`; caller cannot substitute another backend. The old options-based constructor remains behavior-compatible.

## Receipted VOD seek

- P5 plans anchors without HTTP: binary search chooses the video fragment at/before target and independently chooses the audio fragment at/before target. Video fragments are valid anchors only because F2 required and proved first-sample RAP for every admitted video fragment; no guessed keyframe rule exists.
- `SmoothTransactionalVodDemuxer` rebuilds both sources and both concrete demux axes offside. It verifies the same stable track contracts, builds the replacement composite with the same public IDs, then performs one swap. Any source/open/track failure leaves the active composite untouched and a later seek may succeed.
- `ProgressiveDemuxer::new_deferred_receipted_seekable` is an additive neutral constructor: preview is pure, worker execution returns generation-fenced receipts, and the app maps them through the existing provider-neutral `PreparedDemuxSeekPort`. The seek port is consumed only after the normal Ready → authorize → Installed lifecycle.

## App composition and C3

- `app-egui::web_media_open::smooth` is a separate child module, not new logic in the already-large central file. It recognizes only a muxed `TransportFamily::SmoothStreaming` candidate, rejects live/DVR intent, prepares the manifest/catalog, resolves provider-default or semantic component selection against that fresh catalog, and only then builds sources/demux.
- Component catalog generations are app-owned and process-monotonic; they are not derived from extraction/source generations. The normal stream-model finalization revalidates the same fresh catalog before publication, so reopen/settings never silently fall back to another quality.
- The app registers Smooth planner capability only as `OrderedSegments`, injects its existing web `DemuxRegistry` into both axes, and owns all XML/manifest/init/reconstruction/readiness/interleave/receipt budgets. HLS, DASH and progressive branches keep their previous capabilities and ownership.
- S00 has one approved exact static VOD row: muxed ISM/MSS fMP4 with H.264 + AAC-LC. Live/DVR remains `ProfileExcludedProvisional`; therefore no `S36L-*` card or dependency was created. Unknown/private/DRM/other codec or layout constructs remain typed incompatible.

## Focused proof and verification

- `web-media-smooth` tests cover deferred open, stable canonical A/V tracks/duration, pure anchor planning, exact independent A/V fetch anchors, successful receipt and failed-audio transactional rollback followed by successful retry.
- `symphonia-demux` proves the injected registry opens canonical presentation-window audio without a parallel factory. App capability tests prove Smooth ordered-only registration while preserving HLS/DASH shapes; existing C3 tests prove provider-default and semantic fresh-generation finalization.
- Accepted gates: `web-media-smooth` 30/30, presentation-window Symphonia 10/10, `app-egui` 878/878, strict touched Clippy, strict workspace rustdoc, Rust 1.92 workspace MSRV, app without default features, local ISO patch 85/85, guardrail unit tests and refactor guardrails, dependency licenses/sources/bans/machete, fmt and diff check. The full workspace test command had one unrelated timing race in `playlist-discovery`; its exact retry passed immediately. Two earlier parallel local-HTTP fixture races in `web-media-adaptive`/`service-ytdlp` also passed on isolated package rerun.

Related: `mem:media-services/smooth-fragment-sources-s36p3-2026-07-25`, `mem:media-services/presentation-window-transport-s36f3a-2026-07-25`, `mem:player-core/exact-pcm-presentation-window-s36f3b-2026-07-25`, `mem:demux-api/core`, `mem:symphonia-demux/core`, `mem:app-egui/queue-owned-web-open-s23-2026-07-22`.