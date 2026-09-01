# N11 — native Smooth Streaming VOD ingress (2026-09-01)

## Result

Session N11 implements direct HTTP(S) Smooth Streaming VOD ingress for URLs whose final non-empty path segment is case-insensitive `Manifest`. The URL is only a syntactic hint: the existing `smooth-streaming-manifest-core` parser authoritatively confirms the root and existing static H.264/AAC profile. No second parser, catalog builder, fragment runtime, demuxer, decoder path, or extractor-backed runtime was added.

## Ownership and boundaries

- `app-egui::url_service_adapter::native_smooth` owns the syntactic HTTP(S) `/Manifest` classification and constructs a secret-safe, stable-root source intent plus the initial extractor fallback locator.
- `app-egui::media_open::native_smooth::NativeSmoothUrl` owns only the exact stable root, safe label, and opaque source lineage. Fragment/redirect endpoints, headers, cookies, and runtime material never enter durable active-source state or Debug.
- `app-egui::startup_media::native_smooth` owns the initial bounded root fetch, one allowed pre-install fallback decision, fresh catalog identity, startup job cancellation/join, and stable-root reopen attachment.
- `app-egui::web_media_open::smooth` is the app adapter over the existing `web-media-smooth` discovery/open APIs. It builds the provider-neutral catalog, rematches exact semantic component selection on every fresh snapshot, and reuses the existing Smooth fragment/demux runtime.
- `web-media-smooth::SmoothFetchedManifestInput` hands the already fetched root body and its exact adaptive HTTP context to preparation. Preparation validates source generation, selected root provenance, and the current manifest byte budget before parsing, so the first root request is never duplicated.
- `smooth-streaming-manifest-core` now distinguishes a well-formed foreign XML root as `InvalidRoot`; namespaced/private Smooth roots remain `PrivateExtension`, and malformed Smooth/XML remains malformed.

## Admission and failure policy

- Initial direct admission may fall back to yt-dlp only for authoritative `InvalidRoot` (strictly not Smooth) or HTTP 401/403 authentication admission.
- Network/transport failure, cancellation, malformed XML/schema, live profile, DRM, private extension, unsupported codec profile, unsupported native profile, and runtime preparation failures are terminal typed native errors and never trigger extractor fallback.
- A semantic switch/reopen has no fallback locator at all. After installation, endpoint recovery always reopens the stable root and semantically rematches the selected video/audio components against a fresh catalog generation.
- Public secret-free failure categories are `Cancelled`, `Transport`, `InvalidRoot`, `LiveProfile`, `DrmProtected`, `PrivateExtension`, `UnsupportedCodecProfile`, `UnsupportedNativeProfile`, `MalformedManifest`, and `RuntimePreparation`.

## Functional evidence

Hermetic `app-egui` vertical uses the canonical Smooth PIFF H.264/AAC fixtures and a controlled loopback server. With `yt_dlp.enabled=false`, it proves:

- exactly one root `/Manifest` GET per initial open, component switch, and controlled reopen;
- provider-neutral catalog publication and exact semantic audio component switch;
- worker-receipted VOD seek;
- H.264 packets reach the existing FFmpeg decoder and WGPU submit/release path;
- AAC packets reach the production audio decoder and produce nonzero PCM;
- controlled reopen refreshes the stable root, preserves source lineage and semantic selection;
- the injected process spy remains exactly zero throughout open/seek/switch/reopen;
- secret-bearing manifest query does not appear in source/intent Debug.

The vertical cohort passed three ordinary runs, satisfying §6.3 normal feature-source repeatability. Failure vertical proves distinct live/DRM/private/codec/malformed/network/cancel behavior and permits fallback only for a foreign root.

## Verification

PASS:

- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo test -p smooth-streaming-manifest-core -p web-media-smooth --all-targets --all-features --locked` (32 unit + 15 profile + 40 Smooth tests)
- `cargo test -p app-egui --all-features --locked native_smooth --no-fail-fast` (5 tests; final run, vertical cohort 3/3 across session)
- `cargo test -p app-egui --all-features --locked same_item --no-fail-fast` (7 tests)
- `cargo test -p app-egui --all-features --locked controlled_reopen --no-fail-fast` (1 test)
- `cargo clippy -p smooth-streaming-manifest-core -p web-media-smooth -p app-egui --all-targets --all-features --locked -- -D warnings`
- `cargo check --workspace --all-targets --all-features --locked`
- Serena diagnostics for all new/key changed production owners: clean.

Per §6.3, full workspace tests, MSRV, dependency gates, rustdoc, release build, stable coverage, public-network sources, GUI/manual playback, and hardware acceptance were not run; those belong to later gate/acceptance sessions.

## Handoff

Expected local commit message: `feat(smooth): open direct manifests without yt-dlp`. N12 has not started.