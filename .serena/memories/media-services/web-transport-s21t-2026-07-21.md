# S21T neutral web transport / secret / network boundary (2026-07-21)

## Владельцы и dependency shape

- Новый workspace crate `web-media-transport-api` — neutral provider/secret/network contract до первого concrete provider. Разрешённые normal dependencies: только `web-media-core`, `source-core`, `thiserror`; это закреплено refactor guardrail-ом и отдельным regression test.
- В crate намеренно отсутствуют concrete HTTP client/cache/prefetch, yt-dlp/service DTO, demux, queue, player, UI и decoder dependencies. Первый concrete `web-media-http` остаётся S21U.
- `source-core` владеет low-level HTTP values/validation/redaction: `HttpRequestTarget` сохраняет exact caller URL отдельно от WHATWG-normalized scheme/origin/path policy evidence; `HttpPathScope` проверяет segment-boundary path match без публикации raw path; `ValidatedHttpHeaders` использует reqwest header grammar и redacted Debug.
- `source-core::StreamingByteSource` — новый forward-only cancellation-aware byte primitive. Каждое чтение принимает `CancellationToken`; будущий non-Range provider не должен возвращать обычный potentially blocking `std::io::Read` без cooperative cancellation. Seekable path по-прежнему использует `ByteSource`; prefetch по-прежнему принадлежит `media-prefetch`.

## Identity, request и result

- `MediaComponentIdentity` связывает snapshot-local exact `CandidateIdentity`, refresh-stable `SemanticIdentity` и typed `MediaComponentRole`, проверяя одну source lineage.
- `SourceGeneration` отделена от extraction generation. `TransportRefreshRequest` сохраняет provider, semantic identity и component role, требует strictly newer replacement generation. `TransportRegistry::refresh_if_current` принимает current owner generation и typed отклоняет stale refresh до provider call.
- `TransportOpenRequest` содержит exact provider/component/`TransportRequestTarget::{Http,Ftp}`, `MediaPresentation::{Vod,Live}`, source generation, redirect policy, ephemeral secrets, shared cancellation и optional typed `HttpRangeRequestLimit`. FTP opens use `TransportOpenRequest::for_ftp` (empty HTTP secrets, no redirects). `ProviderDescriptor` schemes are `TransportScheme::{Http,Ftp}`. Лимит добавляется named `with_http_range_request_limit`, а не новым позиционным `Option`; default `None` сохраняет поведение direct-media и остальных providers. Neutral API гарантирует только non-zero value и не знает yt-dlp/prefetch.
- `TransportInput` shape невозможных комбинаций не публикует: `Seekable(Box<dyn ByteSource>)` проверяет реальную seekability; `Streaming(Box<dyn StreamingByteSource>)` остаётся forward-only/cancellation-aware. `OpenedTransport` и `RefreshedTransport` получают identity только от registry/caller request, provider не может подменить её.
- Provider registry выбирает только exact `TransportProviderId`; absent provider, unsupported scheme/material/presentation/seekability, auth, transport, refresh, cancellation, redirect rejection, stale generation и provider contract violation остаются разными typed outcomes. Raw implementation error chains/response payload не входят в API.

## Secret и redirect policy

- `SecretRequestContext` ephemeral и не имеет persistence/serde surface. Named builder заранее покрывает validated serialized headers, serialized cookies, primary-resource `request_data`, media-segment query override и encryption-key query override без yt-dlp type leakage.
- S26 extension (2026-07-22): `SecretRequestScope` делегирует origin/path/secure checks shared `source-core::HttpRequestScope`; concrete provider клонирует тот же proof в per-source `ScopedHttpCookieJar`. Initial Cookie сохраняется exact, in-scope Set-Cookie обновляет Range session, а jar read/write дополнительно blocked вне scope. Full details: `mem:media-services/ytdlp-system-auth-s26-2026-07-22`.
- Доступ к material возможен только через `material_for(target, purpose)`; одновременно проверяются normalized origin, segment-boundary path subtree и secure requirement. HTTPS initial target автоматически создаёт SecureOnly scope.
- Request body выдаётся только `PrimaryResource`; segment/key overrides выдаются только соответствующему `SecretRequestPurpose`.
- `RedirectPolicy` использует typed `RedirectHopLimit`/`RedirectHopCount`, exact origin policy и HTTPS downgrade policy. Cross-origin redirect может быть разрешён только как without-secrets candidate; сам `SecretRequestContext` всё равно fail-closed отклоняет cross-origin/path/downgrade target. S27 применяет тот же contract и к поздним redirects последующих seekable Range reads через `source-core::HttpRangeRedirectHandler`: source владеет parsed target/method mechanics/per-read hop count, concrete transport владеет authorization и rematerialization scoped headers; automatic reqwest redirects запрещены.
- Debug/Display/errors скрывают locator userinfo/path/query/fragment, header/cookie/body/query values и candidate format/semantic payload.

## Focused evidence

- `cargo test -p source-core -p web-media-transport-api`: source-core 22 tests + transport API 5 focused tests PASS.
- Покрыты absent/active fake provider, same-origin/path/secure/purpose forwarding, cross-host redirect non-leakage, open cancellation, cancellation-aware stream read, stale и active refresh, secret-safe Debug/errors.
- Strict focused Clippy, rustdoc `-D warnings`, fmt, refactor guardrails + 22 Python guardrail tests, Rust 1.96 workspace check и Rust 1.92 MSRV workspace check PASS.
- Полный workspace all-targets Clippy был запущен, но упирается в unrelated pre-existing `ui-artwork-egui/src/lib.rs` `clippy::items_after_test_module`; изменённые crates strict Clippy проходят.

Связанные знания: `mem:core`, `mem:media-services/core`, `mem:media-services/direct-media`, `mem:media-services/secret-safe-locators-s10b`, `mem:demux-api/core`.