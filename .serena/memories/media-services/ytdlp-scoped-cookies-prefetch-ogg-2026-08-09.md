# YtDlp scoped cookies + Ogg active-prefetch fix (2026-08-09)

## Scope and symptoms

Acceptance row `03 Progressive HTTP — audio-only Ogg` uses the real Wikimedia `Example.ogg`. Two consecutive user-visible failures had independent roots:

1. `transport open unsupported: request material`: pinned `yt-dlp 2026.07.04` emits its separate `cookies` field as flattened response-style records, not as a ready request `Cookie` header.
2. After cookie projection was fixed, Symphonia reported `no suitable format reader found`. The Ogg probe sought near EOF while the second contiguous HTTP prefetch range was already in flight. The old prefetch seek policy cancelled that range, reset the window and downloaded it again. Wikimedia answered the redundant request with 429; Symphonia converted the first source read failure into apparent EOF and replaced the cause with generic unsupported-format text.

Related knowledge: `mem:core`, `mem:media-services/core`, `mem:media-services/ytdlp-system-auth-s26-2026-07-22`, `mem:media-services/progressive-web-hardening-s27-2026-07-22`, `mem:media-services/web-transport-s21t-2026-07-21`.

## Ownership and boundaries

- `service-ytdlp::candidate::request_material::cookies` owns the bounded pinned yt-dlp grammar. It produces one explicit intent: legacy ready `RequestHeader` or individually scoped `ScopedSeeds`. Duplicate/unknown/mixed attributes, missing Domain, conflicting header/field forms and invalid values are typed pre-I/O failures.
- `source-core::HttpCookieSeed` is the neutral redacted carrier/builder for one response-style cookie. `ScopedHttpCookieJar` imports seeds separately and remains the owner of RFC Domain/Path/Secure/expiry matching plus the outer origin/path/downgrade scope.
- `web-media-transport-api::SecretRequestContext` transports ready Cookie header and scoped seeds as distinct fields. HTTP and adaptive providers only project those typed fields into their per-source jars. No cookie material enters serde, config, playlist persistence or UI.
- `media-prefetch` owns fetch scheduling. `ActivePrefetchFetch` stores exact `[start, end)` and its cancellation token under the same mutex. A seekable foreground source may stage its logical cursor inside that active range without cancel/reset/refetch. Buffered seek, out-of-range refetch, cancellation accounting and `NotSeekable` behavior retain their previous semantics.
- `symphonia-demux::ByteSourceFailureObserver` is enabled only during generic eager probe. If probe fails, it restores the first concrete `SourceError`; after successful probe it disables itself so runtime errors keep the old typed source chain. Proven specialized open paths are unchanged.

## Important invariants and tests

- `crates/media-prefetch/src/source/tests/active_fetch.rs`: a delayed fake proves forward seek inside an active range performs no cancellation, source seek or duplicate read; a separate test proves the optimization cannot bypass `NotSeekable`.
- `crates/symphonia-demux/src/symphonia_demuxer/tests/byte_source_failure.rs`: a failing source reaches the public generic constructor as `DemuxError::Io` with downcastable `SourceError`, never as `no suitable format reader`.
- `crates/app-egui/src/web_media_open/content_probe_tests/vorbis.rs`: fake yt-dlp -> request-limited Range origin -> production HTTP/prefetch -> Symphonia Ogg/Vorbis -> production audio decoder -> non-empty PCM. The valid fixture is larger than 64 KiB and the origin permits exactly three requests: initial `Range 0-0` plus two contiguous prefetch ranges. Any old duplicate fetch deterministically receives 429.
- Existing cookie integration proves scoped seed delivery, cross-origin/path/HTTPS gating, per-source isolation, redacted diagnostics and production Ogg/Opus PCM.

## Verification

- Full `scripts/ci-checks.sh tests`: PASS, including 936 app tests and all workspace/doc tests.
- Strict Clippy, strict rustdoc, app without default features, Rust 1.92 MSRV check, fmt, diff check and refactor guardrails: PASS.
- Serena diagnostics are empty for all changed Rust files except the pre-existing rust-analyzer-only unresolved `audio_fixtures` path diagnostic in the app parent test module; Cargo resolves it and full tests pass.
- Final runner-built release live smoke on 2026-08-09 opened real Wikimedia Ogg as one `A_VORBIS` track, created the production Symphonia decoder and CPAL `AudioOutput`, and started the audio stream without runtime errors. Runner exit 124 is the expected 20-second timebox; one selected case remains `MANUAL REVIEW REQUIRED` by the 29-case acceptance contract.
