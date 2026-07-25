# S36P1 — serialized ISM manifest request projection (2026-07-25)

## Ownership and boundary
- `service-ytdlp` now owns the public typed projection from an exact normalized `protocol=ism` candidate to one neutral `TransportOpenRequest` for the VOD client manifest. Future `web-media-smooth` receives only neutral S21T request state and never sees yt-dlp DTOs.
- `YtDlpSmoothManifestRequestMaterial` is borrowed/sealed with private raw references and an intent-named fetch-target accessor. Debug and all errors omit target/query/userinfo/header/cookie values.

## Exact admitted material
- Allowed serialized material is only `url`, `manifest_url`, validated HTTP headers and serialized cookies. At least one target is required; `manifest_url` is authoritative; when both exist they must be byte-exact equal. A failed authoritative request never falls back to the other field.
- Target must be absolute hierarchical HTTP(S). Serialized fragments, fragment base URL, DASH periods, inline HLS, segment/key query overrides, HLS AES, RTMP and HTTP range request limit are distinct typed incompatibilities rather than silently ignored state. Existing competing Cookie serialization remains its exact typed S26 failure.
- Candidate must have exactly one muxed request component and exact normalized SmoothStreaming + fragmented ISO-BMFF + H.264 + AAC shape. Request is VOD with muxed identity; live/DVR is not admitted.

## Secrets and lifecycle
- The projection preserves exact/semantic identity, source generation and cancellation. It builds the existing path-scoped `SecretRequestContext`, ephemeral serialized cookies and validated headers; cross-origin redirects receive no secrets.
- No network I/O, manifest parse, provider registration, fragment execution, demux, seek or app/player work occurs in P1. Compatibility profile/REPORT remain unchanged.

## Verification
- Focused tests cover target precedence/equality/missing/malformed/non-HTTP, every unsupported field, Cookie conflict, redaction, checked-in `target-ism-fmp4`, identity/generation/cancellation, scoped auth/cookie and cross-origin stripping, plus non-ISM/layout/component/container/codec rejection.
- Accepted checks: service-ytdlp 70 unit + 5 profile/integration tests, strict all-features Clippy, strict rustdoc, Rust 1.92 check, dependency/toolchain/refactor guardrails, fmt, diff and Serena diagnostics.

Next owner: S36P2 creates `web-media-smooth` for manifest fetch/parse, validated quality mapping and neutral C3 catalog; it must consume this neutral request only.