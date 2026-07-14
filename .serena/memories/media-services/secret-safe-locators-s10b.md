# Session 10B: secret-safe URL/service locator boundaries

Session 10B завершена PASS 2026-07-14. Эта memory дополняет `mem:core`, `mem:media-services/core`, `mem:media-services/direct-media` и `mem:playlist/core`.

## Ownership и API
- `service-direct-media::DirectMediaUrl` — service-owned typed locator. `parse_direct_media_url` валидирует direct http(s)+extension policy, но сохраняет исходную строку exact, включая signed/functional query и percent-encoding. Raw identity доступна только через `expose_secret_for_open` и `expose_secret_for_persistence`; `Debug`/`Display` и `safe_label` скрывают userinfo/path/query/fragment.
- `open_direct_media_url` и `open_direct_media_url_with_options` принимают `&DirectMediaUrl`, а не raw `&str`. `DirectMediaOpenError` сохраняет прежние typed категории, но URL context теперь typed/redacted и invalid syntax не отражает input.
- `service-youtube::YoutubeMediaLocator` создаётся только через `parse_youtube_media_locator`. YouTube host allowlist и normalize policy принадлежат исключительно service-youtube. Normalization v1 удаляет только `utm_*`, `si`, `feature`; сохраняет unknown и functional `v`, `t`, `start`, `end`, `list`, `index`, а повторный parse идемпотентен. Raw normalized identity доступна только через intent-named open/persistence accessors.
- Все public YouTube resolve/open/reopen API принимают `&YoutubeMediaLocator`. Internal yt-dlp process получает raw только непосредственно при command construction. `RefreshContext` хранит typed locator.
- `YoutubeDirectStreamUrl` хранит exact transient signed stream URL и раскрывается только для transport open. `YoutubeDirectStreamDescriptor`/candidate/aggregate `Debug` безопасен; `HttpHeader::Debug` скрывает values.
- `source-core::SecretHttpUrl` — service-neutral transport locator. `HttpRangeSourceConfig`, `HttpRangeSource` и URL-bearing `SourceError` используют его; tracing и `Debug` redacted, reqwest error chains проходят `without_url()`, HTTP fingerprint содержит только hash identity, не raw URL.
- `app-egui::StartupUrlLocator` — service-neutral type-erased adapter contract. Общий traversal обходит единую таблицу service-owned classifiers и не содержит YouTube enum/host/query parser; typed adapter запускает существующий startup job. Добавление будущего URL service требует нового adapter implementation/registration, но не изменения общего traversal/locator shape. Mapping `StartupUrlLocator` ↔ `playlist_core::SecretUrlLocator` использует только intent-named exposure и тот же registry без второго parser-а; поэтому `app-egui -> playlist-core` является намеренной однонаправленной domain dependency. `InitialMedia`, startup jobs и `ActiveMediaSource` хранят typed service locators; settings rebuild повторно использует их без reparsing.

## Privacy invariants
- Никакой automatic `Debug`/`Display`, tracing field, UI/source label, typed error context, reqwest source chain, HTTP fingerprint или yt-dlp stderr summary не содержит raw userinfo/path/query/fragment.
- yt-dlp stdout/stderr остаются внутренними process bytes; `ProcessOutput::Debug` показывает только размеры, non-zero/timeout errors используют bounded `stderr redacted (N bytes)`.
- Direct URL нельзя пересобирать через `query_pairs_mut`: exact signed identity сохраняется. YouTube может пересериализовать только при удалении service-known tracking parameters и затем обязан быть idempotent.

## Verification
- Hermetic tests: source-core 18 PASS; service-direct-media 9 PASS + 1 manual ignored; service-youtube 33 PASS + 4 manual ignored; app-egui 286 PASS.
- Strict focused Clippy, `cargo fmt --all --check`, `cargo +1.96.0 check --workspace --locked`, refactor guardrails, `git diff --check`, Serena references/diagnostics PASS.
- Dependency graph получил только ожидаемую app → `playlist-core` связь для domain mapping; новых external packages нет.

Следующая разрешённая session — только 10C. Session 10B не добавляла playlist confirmation/persistence UI, network test I/O или media-open coordinator.