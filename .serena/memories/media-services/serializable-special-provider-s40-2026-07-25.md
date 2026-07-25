# S40 serializable special-provider expansion gate (2026-07-25)

## Result

S40 завершён как доказанный no-op. В canonical S00 profile нет отдельной `PublicSerializable` special-provider target row и, следовательно, нет stable row ID для `S40P-*` card. `PublicSerializable` у scalar `protocol` доказывает только JSON-форму строки, а не воспроизводимый provider descriptor.

Все 13 текущих target rows уже принадлежат concrete S22–S39 sessions. S41 сохраняет dependency только на завершённый S40 gate и не получает дополнительных `S40P-*` dependencies.

## Exclusion boundary

Exact identities `bunnycdn`, `soopvod`, `niconico_live`, `fc2_live`, `websocket_frag` остаются в `special_private_state_excluded`. Для них нет отдельной S00 target row и exact deterministic transport-to-demux fixture. Representative synthetic fixture доказывает lossy WebSocket `repr` и private refresh/ping state; он не является per-provider implementation evidence.

`downloader_options.ws` и `fragments(generator_or_repr)` остаются `RequiresLiveExtractorState`. `_bunnycdn_ping_data|_cookie_refresh_params` остаётся `private_api_target_row_excluded` с `future_session = none_without_public_serializable_profile_extension`. Production provider/API/dependencies, Python helper и IPC не добавлялись.

Будущее расширение сначала обязано добавить отдельную S00 public-serializable target row со stable ID и exact fixture. Затем создаётся и обсуждается owner-specific `S40P-<stable-row-id>` card с descriptor schema, provider owner, transport→demux mapping, minimal direct dependencies и deterministic refresh/cancel/stale/secret tests. VOD card не получает S31L/S35S; live/DVR card зависит от них явно.

## Focused evidence and tests

- `crates/service-ytdlp/tests/compatibility_profile_s40.rs`: отсутствие S40/S40P target rows, exact excluded alias family, live-state classifications, private request decision и secret-safe non-reconstructible fixture.
- `crates/web-media-core/src/normalized.rs::tests::transport_aliases_map_to_manifest_families`: все пять exact identities → `KnownExcludedTransport::PrivateLiveState`.
- `crates/service-ytdlp/src/candidate/tests.rs::unknown_and_profile_excluded_candidates_remain_visible`: все пять rows → `ProfileExclusionReason::RequiresLiveExtractorState`.
- `crates/service-ytdlp/src/candidate/tests.rs::non_reconstructible_request_material_remains_visible_and_redacted`: оба private fields → `PrivateExtractorStateRequired`, synthetic values отсутствуют в Debug.

Verification: Rust 1.96 focused tests PASS (`web-media-core` 1 focused; `service-ytdlp` lib 78; S00 4; S40 2), relevant all-targets/all-features check and strict Clippy PASS, fmt, refactor guardrails, diff check and Serena diagnostics PASS.