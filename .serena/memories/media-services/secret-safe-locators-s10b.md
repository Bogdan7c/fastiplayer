## S31 adaptive secret reuse (2026-07-23)

- `web-media-adaptive` не создаёт durable locator/auth surface: он принимает S21T `TransportOpenRequest`, применяет scoped manifest/segment purpose material, manual redirect authorization и ephemeral cookie jar. Cross-origin hop монотонно снимает header/query secrets; raw targets/material не попадают в Debug/errors. См. `mem:media-services/adaptive-transport-s31-2026-07-23`.

# Secret-safe URL/service locator boundaries — актуально 2026-07-17

Эта memory дополняет `mem:core`, `mem:media-services/core`, `mem:media-services/direct-media` и `mem:playlist/core`.

## Ownership и exact identity

- `service-direct-media::DirectMediaUrl` принимает direct HTTP(S)+supported extension, сохраняет exact signed/functional identity и раскрывает raw строку только через intent-named open/persistence accessors. Этот service остаётся первым URL adapter-ом.
- `service-ytdlp::YtDlpMediaLocator` принимает любой absolute HTTP(S) URL с host и сохраняет исходную строку byte-for-byte. Query/userinfo/path/fragment не нормализуются и не удаляются; разные exact URL, включая разные YouTube tracking parameters, являются разными playlist identities.
- `YtDlpMediaLocator::{Debug,Display,safe_label}` показывают только host. Invalid syntax/scheme errors не отражают input. Generic yt-dlp persistence по утверждённой policy не требует дополнительного acknowledgement даже при query/userinfo; это означает, что exact token может попасть в playlist-state, но не в UI/logs/diagnostics.
- `YtDlpDirectStreamUrl` хранит transient signed stream URL и раскрывается только transport-у. `YtDlpDirectStreamDescriptor`, selected identity и candidate stream id имеют redacted/opaque Debug; `HttpHeader::Debug` скрывает values.
- `source-core::SecretHttpUrl` остаётся service-neutral transport locator. Reqwest chains проходят `without_url()`, URL-bearing errors/tracing/fingerprint не содержат raw URL.

## App composition

- `app-egui::StartupUrlLocator` остаётся type-erased service-neutral adapter contract. Registry register order: direct-media -> yt-dlp. Любой оставшийся валидный HTTP(S) URL classified как `YtDlp`; фактическая поддержка определяется фоновой extraction/admission попыткой.
- После classification adapter фиксирован для request/reopen/suspend/settings rebuild. Ошибка direct-media open не вызывает второй скрытый open через yt-dlp.
- `InitialMedia`, `MediaOpenSourceRequest`, prepared descriptor, `ActiveMediaSource`, startup jobs и playlist metadata source используют `YtDlp` variants и typed locator. Mapping к `playlist_core::SecretUrlLocator` использует exact persistence accessor и тот же registry без второго URL parser-а.

## Privacy/error invariants

- Никакой automatic `Debug`/`Display`, tracing field, UI/source label, typed error, reqwest chain, HTTP fingerprint или process stderr summary не содержит locator userinfo/path/query/fragment, signed direct URL/header values либо extractor format identity.
- `YtDlpServiceError` сохраняет категории cancellation/timeout/process/extractor/collection/compatibility/transport/demux без raw payload. Non-zero extractor error сообщает только bounded stderr byte count.
- System `yt-dlp` продолжает читать собственные config/cookies; app не хранит отдельные credentials и не добавляет `--ignore-config`. Начиная с S26, effective serialized headers/cookies свежей extraction generation маппятся только в origin/path/secure-scoped `SecretRequestContext` и per-source ephemeral Set-Cookie jar без config/state/export/logging surface; см. `mem:media-services/ytdlp-system-auth-s26-2026-07-22`.

Focused tests: `crates/service-ytdlp/src/locator.rs`, service descriptor/process tests, `crates/app-egui/src/url_service_adapter.rs`, media-open redaction tests и playlist metadata stale/exact tests.

## S05 playlist-io exact URI note (2026-07-20)
- `playlist-io` generic M3U absolute hierarchical URI validates syntax через `url::Url`, но передаёт в `SecretUrlLocator` exact caller string, а не reserialized/normalized URL. Только relative URI получает newly resolved canonical identity. `M3uDocumentSource` и parse errors/Debug redacted.
- Parser не делает scheme admission/fetch: hierarchical draft остаётся app registry input; opaque/non-network forms и remote-authority `file:` становятся typed bounded issues. HLS child/segment URI никогда не публикуются как queue rows. Full boundary: `mem:playlist/io-s05-m3u-hls-2026-07-20`.


## S10 playlist export preflight note (2026-07-20)
- `playlist-io::PlaylistExportLocatorPolicy: Send + Sync` — новая neutral service/app-owned граница: exact `SecretUrlLocator` и stable `ServiceDurableReopenPayload` могут попасть в M3U8/XSPF только как validated portable HTTP(S) `PortablePlaylistExportUrl` с explicit `Public|SensitiveDurableIdentity` classification.
- Opaque service payload без owner-approved portable URL typed rejected; operational signed URL не является fallback и transport headers/cookies/candidate IDs отсутствуют в S10 type surface. Errors/Debug не содержат URL/path/service payload.
- Aggregated `PlaylistExportSecretClassification` считает реально serialized track и XSPF group-root locators для будущего S11 confirmation/user-only writer policy. Full contract: `mem:playlist/io-s10-export-2026-07-20`.

## S15A locator/admission override (2026-07-20)

- Более раннее утверждение этой memory о HTTP(S)-only `YtDlpMediaLocator` заменено: pure parser теперь принимает exact S00 vocabulary `http`/`https`/`ftp`/`ftps`/`rtmp`/`rtmpe` и хранит typed `YtDlpInputScheme`; иные variants не alias-normalized.
- Composition availability остаётся отдельной app boundary: HTTP(S) admitted по прежнему direct-first/fallback contract, а FTP(S)/RTMP требуют exact registered `ImplementedYtDlpInputProviderCapability`. Production extended registration пуст до готовности S37/S39 provider fixtures.
- Более ранний no-prompt persistence policy для generic yt-dlp URL заменён roadmap-wide aggregated policy: любой exact locator с non-empty query либо userinfo требует sensitive durable-locator acknowledgement и для persistence, и для export. Raw identity по-прежнему раскрывается только intent-named open/persistence accessors.
- `Debug`/`Display`/safe errors для active и unavailable extended schemes не содержат userinfo/path/query/fragment. Pending confirmation хранит opaque yt-dlp metadata continuation, а UI видит только bounded safe label/reasons.

## S16 service-owned durable child payload (2026-07-20)

- Stable extracted child/delegation identity теперь классифицирует и кодирует только `service-ytdlp::topology::reopen`: owner/version/8 KiB bound explicit, material kinds ограничены webpage/original/extractor identity, payload/error Debug redacted. App не создаёт собственный schema и только исчерпывающе переводит service descriptor в neutral `DurableReopenLocator`.
- Exact root остаётся acknowledged URL identity; extracted child остаётся opaque service identity, поэтому direct-media-first registry не может случайно изменить service при reopen. Ephemeral format/manifest/fragment/key/signed/header/cookie/auth/session variants отсутствуют в service descriptor и fail-closed в neutral constructor. Full mapping: `mem:app-egui/ytdlp-topology-drafts-s16-2026-07-20`.
