# Obsolete: Hover Budget Diagnostics

- OBSOLETE since 2026-07-03: hover budget settings, resolver/admission DTOs, backend diagnostics providers, app preflight telemetry, FFmpeg software-hover budget ownership, and VA shared-hover admission were removed.
- `video-backend-api::StartedVideoBackend` no longer carries a hover budget diagnostics provider. `frame-server-core` no longer exports hover budget types.
- Do not reintroduce hover pool/thread settings or pairwise hover-vs-playback budget rules. Current frame-server config exposes only live-scrub fields.