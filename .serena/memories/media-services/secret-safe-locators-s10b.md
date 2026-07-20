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
- System `yt-dlp` продолжает читать собственные config/cookies; app не хранит отдельные credentials и не добавляет `--ignore-config`.

Focused tests: `crates/service-ytdlp/src/locator.rs`, service descriptor/process tests, `crates/app-egui/src/url_service_adapter.rs`, media-open redaction tests и playlist metadata stale/exact tests.

## S05 playlist-io exact URI note (2026-07-20)
- `playlist-io` generic M3U absolute hierarchical URI validates syntax через `url::Url`, но передаёт в `SecretUrlLocator` exact caller string, а не reserialized/normalized URL. Только relative URI получает newly resolved canonical identity. `M3uDocumentSource` и parse errors/Debug redacted.
- Parser не делает scheme admission/fetch: hierarchical draft остаётся app registry input; opaque/non-network forms и remote-authority `file:` становятся typed bounded issues. HLS child/segment URI никогда не публикуются как queue rows. Full boundary: `mem:playlist/io-s05-m3u-hls-2026-07-20`.
