# Rustiplayer architecture

Rustiplayer separates playback policy, media transport, decoding, frame lifetime, rendering, and the desktop shell. The application is the composition root; the playback core consumes contracts rather than concrete GPU or decoder implementations. This document describes the current implementation, not the 1.0 roadmap.

For supported media and measured outcomes, use the [compatibility matrix](docs/web-media-compatibility-matrix.md) and [N15 acceptance](docs/native-web-ingress-n15-acceptance.md). More detailed engineering documents are indexed in [docs/README.md](docs/README.md); most are in Russian.

## Owners and boundaries

| Responsibility | State owner / implementation | Boundary |
| --- | --- | --- |
| Desktop UI and composition | [app-egui](crates/app-egui/src), egui/winit | User intents, source preparation, concrete backend/renderer selection, window lifecycle |
| Settings schema and transactions | [config](crates/config/src), [settings-core](crates/settings-core/src), [rustiplayer-settings](crates/rustiplayer-settings/src) | Validated descriptors, typed application routes, persistence and rollback |
| Queue and durable identity | [playlist-core](crates/playlist-core/src), [playlist-state](crates/playlist-state/src), playlist-io/discovery | Queue edits, import/export, restore, source identity |
| Byte sources and transport | [source-core](crates/source-core/src), web-media-http/ftp, media-prefetch | Bounded reads, seek/cancellation, transport accounting |
| Web discovery and source lifecycle | [web-media-core](crates/web-media-core/src), protocol-specific web-media crates | Provider-neutral catalogs, semantic selection, stable-root reopen/recovery |
| Container and stream discovery | [demux-api](crates/demux-api/src), symphonia-demux, mpeg-ts-demux, flv-demux | Encoded packets, timing, track metadata and decode-start evidence |
| Playback scheduling and resource ownership | [player-core](crates/player-core/src) | Session/worker commands and receipts; pipeline-owned decoder, audio and render-resource lifetimes |
| Video backend contracts | [video-core](crates/video-core/src), [video-backend-api](crates/video-backend-api/src) | Capability probing, decode input/output, completion and release |
| Concrete video decode | [video-vaapi](crates/video-vaapi/src), [video-ffmpeg](crates/video-ffmpeg/src) | VA-API hardware or FFmpeg software implementation behind neutral contracts |
| Frame format and presentation leases | [video-frame-contract](crates/video-frame-contract/src), [video-present-core](crates/video-present-core/src) | Exact layout/bit depth/transfer path and retained-frame lifetime |
| Audio | [audio](crates/audio/src), audio-core, audio-timestretch, audio-signalsmith | Decode, processing, output and playback clock |
| GPU video rendering | [render-core](crates/render-core/src), [render-wgpu-video](crates/render-wgpu-video/src) | Materialize frames, GPU color conversion/tone mapping, viewport and render diagnostics |
| Device, surface and presentation | [render-wgpu-shell](crates/render-wgpu-shell/src) | WGPU device/surface, video + egui composition, submit and present |

The [workspace manifest](Cargo.toml) records the actual dependency graph. The seven patched upstream crates are outside the first-party workspace and have separate manifests, locks, licenses, and validation; see the [patch inventory](docs/dependency-patches.toml).

## Source opening and playback lifecycle

The application prepares a source through local or web owners, discovers tracks, selects a compatible video plan, and installs the result through the playback boundary. Preparation is not proof of playback: successful consumer tests continue through decode to a submitted/read-back frame or nonzero PCM and an advancing audio clock.

Protocol owners retain their own network, manifest, refresh, and seek invariants. The neutral web catalog exposes source selection intent without leaking extractor-specific implementation types into queue or settings consumers. Supported direct sources take native transport paths; page resolution can use the system yt-dlp adapter.

Installation and restore have distinct outcomes. The strong `Installed` barrier determines when prepared resources become active; subsequent position/track/intent restoration must still complete or report a partial failure. Generation and media-instance identities prevent stale work from replacing a newer source. Old owners and retained frames must remain valid until their consumers release them.

`PlaybackPipeline` owns playback resources behind intent methods; session/tick code must not reach into its queues or decoder fields. Backpressure, absent resources, fatal errors, and successful no-ops remain distinct where callers need them. The [pipeline modules](crates/player-core/src/pipeline) separate video decode, audio, media slots, render resources, and retired decoders.

## Video frames and GPU ownership

A selected video path must satisfy both the stream requirement and the decoder/renderer capability intersection. The frame contract describes layout, bit depth, chroma subsampling, and transfer path. A decoder advertising a codec is insufficient if the renderer cannot consume its exact output.

- **Hardware path:** video-vaapi decodes to supported VA surfaces; compatible DMA-BUF storage is imported by render-wgpu-video. Zero-copy here means avoiding a CPU pixel copy on this compatible transfer path, not eliminating all synchronization or driver work.
- **Software path:** video-ffmpeg produces supported host-planar YUV frames, uploaded and rendered through WGPU. Software decoding does not remove the GPU requirement.
- **Shared lifetime:** presentation retains explicit frame leases. Decoder resources cannot be recycled until the relevant retained consumers and GPU completion/release path permit it. Renderer recreation must respect outstanding resources and renderer generations.

`player-core` does not depend on concrete WGPU/VA-API/FFmpeg implementations. `render-wgpu-video` does not depend on the concrete decoder crates; the application wires backend handles and materializers together. The [refactor guardrails](scripts/check-refactor-guardrails.py) enforce these dependency boundaries.

CPU readback is used by integration tests to inspect rendered output; it is not a production CPU rendering fallback.

## Color and presentation

Decoded metadata and the frame contract reach the rendering boundary together. GPU shaders perform YUV-to-RGB conversion and, on supported HDR paths, BT.2446-C HDR → SDR tone mapping. The verified P010 path converts BT.2020/PQ input to SDR BT.709 output. Native HDR monitor output is not implemented.

The video renderer owns color processing, aspect/orientation handling, and viewport clipping. The shell owns the WGPU surface/device and final video/UI composition, submission, and presentation. It preserves egui texture lifetimes through submission. A frame submitted to a headless adapter proves less than successful presentation on a particular physical display; acceptance reports distinguish them.

## Audio, timeline and diagnostics

Audio owners decode and process PCM and drive the production audio clock. The playback core coordinates packet windows, audio/video timing, seek settlement, and EOF/drain behavior. Video, audio, and network sources can progress asynchronously; queue admission and restore must use the appropriate consumer evidence rather than a generic ready flag.

Diagnostics are produced at their owning boundaries: transport request/byte accounting, decode/completion status, seek timing, audio progress, and renderer submissions. The app presents those observations; it must not infer successful playback solely from a URL opening or a decoder returning a frame.

## Settings while running

The [application-contract matrix](crates/rustiplayer-settings/src/application_contract.rs) maps editable settings to owners and intent-based application mechanisms. The [app transaction adapter](crates/app-egui/src/settings_runtime/transaction.rs) connects those contracts to runtime owners.

Applying settings validates the draft, checks generation/busy conditions, applies runtime routes in order, persists atomically, and finalizes committed state. Failure compensates completed work in reverse order; apply and rollback errors remain separately visible. Busy/conflict outcomes preserve the draft for an explicit retry rather than silently queuing changes.

Some changes are in-place updates or visual previews. Others recreate audio output, reopen the source, rebuild the video pipeline, or recreate the renderer. “Settings while running” does not mean every change is interruption-free or every policy affects an already-started operation. Source reconfiguration waits for correlated installation and restore; enqueueing a command is not successful application.

## Trust boundaries

Media files and network manifests are untrusted. Rust ownership and modular APIs reduce the scope of mistakes, but do not eliminate parser bugs, unsafe code, native-library vulnerabilities, or GPU-driver risks.

- Bounded manifest readers, transport budgets, cancellation, and typed errors belong to their source/parser owners.
- Native-to-extractor fallback is owned by the app and restricted to allowlisted causes before installation. Cancellation, ordinary network failures, malformed input, decoder/render failures, and post-install operations must not silently trigger a new extractor path.
- Temporary resolved endpoints, headers, and cookies do not belong in durable playlist state. The exact locator explicitly confirmed by the user is durable identity, so secrets embedded in that locator can still be persisted. Do not share such state without review.
- System yt-dlp configuration/plugins/cookies are trusted external environment, with side effects outside the app's guarantees. FFmpeg is used for software video decode, not as a hidden network or demux fallback.
- Upstream patches preserve their licenses and safety obligations. Dependency checks, FFI review, and hardware validation complement Rust's type system.

Read [operational errors](docs/web-media-operational-errors.md), [dependency policy](docs/continuous-integration.md), and the [panic/invariant policy](docs/panic-invariant-policy.md). Follow [SECURITY.md](SECURITY.md) for private vulnerability reporting and its planned launch status.

## Validation map

| Invariant / behavior | Existing enforcement or evidence |
| --- | --- |
| Concrete backend independence and module size | [Architectural guardrails](scripts/check-refactor-guardrails.py) |
| Correct frame representation | [Frame-contract tests](crates/video-frame-contract/src), [render tests](crates/render-wgpu-video/src) |
| Settings apply, failure, compensation, busy/conflict | [Settings runtime tests](crates/app-egui/src/settings_runtime/tests.rs), [application contracts](crates/rustiplayer-settings/src/application_contract.rs) |
| Sources reach video/audio consumers | N14A/N14B suites and cross-source regression documented in [N15](docs/native-web-ingress-n15-acceptance.md) |
| Seek lands through decode and rendering | [CI vertical seek acceptance](.github/workflows/ci.yml), [manual regressions](docs/manual-media-regressions.md) |
| Actual device playback | [N15 hardware evidence](docs/native-web-ingress-n15-acceptance.md); [manual hardware workflow](.github/workflows/hardware-acceptance.yml) |
| Stable coverage and reproducible gates | [Coverage policy](docs/code-coverage.md), [CI commands](docs/continuous-integration.md) |

New boundary tests must exercise absent resources, active fakes/stubs, errors, accounting edges, and state the boundary must not own. Successful playback claims need consumer-level tests. Existing acceptance is scoped to the recorded revision, fixture, and machine; it is not a blanket claim about every GPU, URL, codec, or UI transition.
