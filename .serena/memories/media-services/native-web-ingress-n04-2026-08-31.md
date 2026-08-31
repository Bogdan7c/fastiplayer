# Native web ingress N04: unified app web envelope (2026-08-31)

## Commit and scope

- Commit message: `refactor(app): introduce unified active web media source`. Exact hash is recorded in the session handoff because this memory is included in that same commit.
- Scope is N04 only: `app-egui::media_open` source/request/descriptor/preparation boundaries plus integrations required to compile. URL sidebar and picker action behavior remain for N05A/N05B.
- Remote preflight after `git fetch origin`: `origin/main...main = 0 0` before implementation.

## Architecture

- `ActiveMediaSource` has one web variant: `Web(WebMediaSourceIntent)`; local files and the neutral playback-window wrapper remain separate.
- `MediaOpenSourceRequest` has one web variant: `Web(WebMediaOpenRequest)`; adapter-specific direct/native-HLS/extractor payloads are private to `media_open::web`.
- `PreparedMediaDescriptor` has one web variant: `Web(PreparedWebMediaEnvelope)`; local and caller-prepared descriptors remain distinct.
- `WebMediaSourceIntent` owns durable neutral facts: actual `WebMediaIngressKind`, exact VOD/live `WebMediaPresentationKind`, source-owned `WebMediaRecoveryStrategy`, optional extractor reason, neutral selection, and reconstructible root intent. It never receives demux/runtime handles or temporary child endpoints.
- `PreparedWebMediaEnvelope` owns immutable Installed descriptor facts: tracks, duration, metadata, source intent, safe label, optional playback window, and the runtime-only VOD recovery attachment. `active_source()` intentionally clones only the durable source intent/window, so endpoint-bearing recovery material cannot enter `ActiveMediaSource`.
- `compose_prepared_web_media` is the single player-facing web composition boundary for direct, native HLS, and extractor paths. Named attachments preserve worker-receipted versus authoritative-post-target seek semantics, live timeline, playback window, and native initial position without collapsing distinct errors.
- Active source/request internals use private boxed adapter dispatch, keeping the outer enums compact without exposing provider types.
- Controlled reopen rebuilds from the stable neutral intent/selection. Native exact reopen cannot invoke extractor fallback; extractor reopen performs fresh extraction/rematch; direct reopen needs no adaptive capability snapshot.
- A typed compatibility bridge remains only for existing settings/sidebar/picker consumers and must be removed by N05B before G1, as allowed by the plan.

## Preserved invariants

- Strong install/authorization/compensation ownership is unchanged.
- Local-file preparation remains its own fingerprinted envelope.
- Extractor playlist metadata, neutral selection, catalog attachment, presentation kind, invocation reason, live timeline, seek port, playback window, and VOD recovery attachment survive the app preparation boundary.
- Native HLS retains authoritative post-target seek and prepared initial-position semantics.
- Direct/native/extractor debug output and safe labels are redacted; no temporary endpoint is persisted in active source.
- Cancellation is checked before adapter I/O and after provider preparation; existing typed failure classification remains intact.

## Tests and verification

- `cargo fmt --all -- --check`: PASS.
- `git diff --check`: PASS.
- `cargo check -p app-egui --all-targets --no-default-features --locked`: PASS.
- `cargo clippy -p app-egui --all-targets --no-default-features --locked -- -D warnings`: PASS.
- `cargo check --workspace --all-targets --all-features --locked`: PASS.
- Three final runs of `cargo test -p app-egui --no-default-features --locked media_open::`: each PASS, 110/110. This cohort includes absent attachments, fake seek/timeline composition, timeline/window error, pre-I/O cancellation, controlled reopen, secret-safe debug, strong-install behavior, and real HTTP/yt-dlp content-probe paths reaching production nonzero PCM.
- Additional focused owners: URL adapter 11/11, same-item switch 3/3, web stream model 15/15, startup media 21/21.
- Public-media, GUI, and hardware scenarios: NOT RUN in N04; the plan reserves them for G1/G2/G3.

## Next boundary

- Stop before N05A. N05A should migrate URL-sidebar intent/actions onto the neutral request/source boundary without changing N04 ownership.
- N05B must migrate picker/catalog action consumers and delete `ExtractorSourceBridge` / `WebMediaSourceAdapterBridge` compatibility projections before G1.

See also `mem:core`, `mem:app-egui/media-open-coordinator-s10c`, `mem:media-services/cross-provider-integration-s41-2026-07-25`, `mem:media-services/native-web-ingress-n03-2026-08-31`, and relevant recovery/same-item memories.
