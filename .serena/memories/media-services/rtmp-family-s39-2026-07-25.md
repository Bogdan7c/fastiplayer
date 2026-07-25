# S39 exact RTMP family gate (2026-07-25)

## Итог

S39 завершён как доказанный no-op: ни один exact RTMP wire variant не получил production provider/dependency/capability. Canonical S00 aggregate Target `rtmp-family-flv` сохраняется только как `rtmp_rtmpe_or_rtmp_ffmpeg_identity_only` inventory evidence. Checked-in synthetic format/request-material fixtures доказывают сериализацию identity, FLV hints и public RTMP fields, но не handshake/chunk/message/play wire behavior.

## Exact profile decisions

- `rtmp-plain-wire`: `ProfileExcludedProvisional` до отдельной exact S00 Target row и deterministic local handshake/chunk/message/play fixture.
- `rtmpe-encrypted-wire`: `ProfileExcludedProvisional` до отдельного настоящего RTMPE crypto handshake + encrypted payload fixture.
- `rtmp-ffmpeg-pseudo-protocol`: жёсткий `ProfileExcluded`; это extractor/downloader identity, не wire protocol, hidden FFmpeg fallback запрещён.
- `rtmps`, `rtmpt`, `rtmpte`: отдельные `ProfileExcludedProvisional`; они не aliases plain RTMP и требуют собственных TLS/tunnel/crypto fixtures.
- Любой иной variant остаётся fail-closed и не нормализуется в `rtmp`.

## Сохранённые boundaries

- `service-ytdlp` продолжает exact parsing `rtmp`/`rtmpe` и private bounded normalization `YtDlpRtmpRequestMaterial`; public RTMP transport projection/accessor не добавлен.
- S21T не получил RTMP target/scheme/provider.
- app-egui production S15A registry продолжает регистрировать только implemented FTP/FTPS extended providers; `rtmp`/`rtmpe` возвращают typed `ImplementedProviderUnavailable`.
- S30 `flv-demux` остаётся demux-only; RTMP network state туда не перенесён.
- S31L/player-core не менялись: live/no-DVR binding появится только после wire-approved provider evidence.
- Dependency graph и FFmpeg decode-only boundary не менялись.

## Evidence и tests

- Machine evidence: `crates/service-ytdlp/compatibility/2026.07.04/profile.json`.
- Human rationale: `crates/service-ytdlp/compatibility/2026.07.04/REPORT.md`.
- Focused gate: `crates/service-ytdlp/tests/compatibility_profile_s39.rs` проверяет aggregate identity-only semantics, exact inventory, отсутствие exact Target rows, statuses/transports/reasons exclusions.
- Existing admission: `crates/app-egui/src/url_service_adapter/tests.rs::extended_s00_schemes_require_exact_implemented_provider_capability` доказывает production unavailability и exact fake capability isolation.
- Verification: full `service-ytdlp` tests, strict all-targets Clippy, focused app admission, Rust 1.96 locked workspace check, refactor guardrails, fmt, diff-check и Serena diagnostics проходят.

Читайте вместе с `mem:media-services/core`, `mem:media-services/secret-safe-locators-s10b`, `mem:flv-demux/core`, `mem:player-core/core`.