# AUD-009: bounded VOD endpoint recovery (2026-08-23)

## Verification

- A separate read-only subagent verified production wiring before implementation and changed no files.
- Confirmed affected paths: progressive HTTP, HLS VOD, DASH VOD, Smooth VOD and HDS VOD. Late signed-URL 401/403/404/410 had no common logical-source re-extraction caller. Existing HLS/DASH live recovery remains separate and was not defective.

## Ownership and boundaries

- `web-media-transport-api::EndpointExpirySignal` is the provider-neutral fact: `MediaComponentIdentity`, `SourceGeneration`, resource class and typed reason. It contains no locator, headers, cookies or auth material.
- `TransportOpenRequest` carries an optional `Arc<dyn EndpointExpiryObserver>`. Progressive `web-media-http` observes late range-read statuses. `web-media-adaptive` observes manifest/clock/media/init/key fetch statuses; HLS VOD, DASH VOD, Smooth and HDS app composition attach the same candidate observer.
- `VodEndpointRecoveryAttachment` in app-egui is shared by all physical components of one candidate. It starts `Unarmed` so speculative rendition/component probe 404s cannot poison a playable sibling. `prepare_yt_dlp_web_media` arms it only after the candidate and component configuration are finalized.
- Once armed expiry occurs, the demux wrapper returns neutral `TemporarilyUnavailable` and suppresses packets, EOF and track changes from the old physical generation. Receipted seek outcomes from the old runtime are withheld.
- `state::vod_endpoint_recovery` owns logical recovery. Its exact Installed binding is fenced by `MediaInstanceId`, playlist `ActiveMediaIdentity`, the source generation in the signal and the runtime attachment. It stores the reconstructible logical yt-dlp source, never physical endpoint URLs.
- Recovery rebuilds the entire exact semantic candidate through `YtDlpCandidateOpenIntent`, including composed A/V. Therefore separate audio/video URLs are refreshed coherently rather than component-by-component.
- Replacement reuses the existing staged same-lineage strong-open/install barrier. The old physical generation cannot publish after the gate opens, and the fresh candidate becomes observable only after exact Installed.
- Same-lineage restore uses a typed position policy. Ordinary candidate switches use the fresh current position; VOD expiry uses the outstanding `timeline.target_position` when present, otherwise current position. Playback intent, volume and selected tracks remain fresh pre-barrier facts.
- Startup, settings rebuild and suspend/resume preserve rich yt-dlp Installed descriptors and recovery attachments. Direct/local/legacy source-only commits explicitly clear VOD-only recovery state.

## Retry policy and config

- Config schema was bumped from v7 to v8.
- `YtDlpConfig` fields: `vod_endpoint_recovery_enabled`, `vod_endpoint_recovery_max_consecutive_attempts`, `vod_endpoint_recovery_initial_backoff_ms`, `vod_endpoint_recovery_max_backoff_ms`, `vod_endpoint_recovery_stable_reset_ms`.
- Defaults: enabled, 3 attempts, 250 ms initial, 2000 ms cap, 30000 ms stable reset.
- Runtime takes an immutable policy snapshot per claimed expiry. Backoff is capped exponential.
- Only a typed pre-barrier source `PreparationFailed` may retry. Cancel, stale identity, player rejection/failure and post-barrier failure never resurrect media. Stable playback resets the consecutive budget.

## Regression anchors

- `web-media-http::tests::late_progressive_403_publishes_typed_expiry_without_hiding_original_error`: initial progressive bytes succeed, late seek/read receives 403, exact generation/resource signal is emitted and original `SourceError::HttpStatus` survives.
- `web-media-adaptive::tests::adaptive_media_410_publishes_generation_fenced_expiry_signal`: media segment 410 emits the shared adaptive signal.
- `app-egui::web_media_vod_recovery::tests`: old demux publication hold, A/V/resource coalescing, generation visibility and pre-finalization speculative expiry isolation.
- `state::strong_media_open::pending::same_lineage::tests::exact_recovery_position_overrides_only_position_not_fresh_controls`: late-seek position overrides only position.
- `state::vod_endpoint_recovery::tests::retry_policy_is_exponential_capped_and_starts_at_initial_delay`.
- `rustiplayer-config::store::tests::yt_dlp_vod_recovery_policy_bounds_are_validated_as_one_contract`.
- HDS regression `null_codec_provider_default_filters_unsupported_hds_and_opens_playable_catalog` specifically guards the speculative-404 poisoning bug found in self-review.

## Verification

- `cargo test -p app-egui --no-default-features --locked`: 955/955.
- `cargo test -p rustiplayer-config --locked`: 92/92.
- `cargo test -p web-media-adaptive --locked`: 34/34.
- `cargo test -p web-media-http --locked`: 15 unit plus 2 audio and 6 progressive integration tests.
- `cargo test -p web-media-transport-api --locked`: 7/7.
- `cargo +1.96.0 check --workspace --locked`: PASS.
- Strict all-target Clippy for app/config/transport/http/adaptive: PASS.
- Rustfmt check, refactor guardrails, diff check and Serena diagnostics: PASS.

## Known limits

- Existing HLS/DASH live refresh is intentionally not routed through the VOD coordinator.
- No external user-authenticated short-TTL service corpus or real GUI/WGPU manual session was run in this change; hermetic transport/lifecycle tests and existing staged install/render-generation tests are the automated evidence. AUD-013 remains the owner of the broader real renderer-bound seek matrix.
