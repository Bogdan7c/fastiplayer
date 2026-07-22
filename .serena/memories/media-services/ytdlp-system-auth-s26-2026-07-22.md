# S26 — System yt-dlp authorization propagation (2026-07-22)

Связанные знания: `mem:core`, `mem:media-services/secret-safe-locators-s10b`, `mem:media-services/web-transport-s21t-2026-07-21`, `mem:app-egui/queue-owned-web-open-s23-2026-07-22`, `mem:config/schema-v6-ytdlp-migration-2026-07-17`.

## Ownership и production flow

- `service-ytdlp` остаётся владельцем yt-dlp serialized request material. Progressive HTTP mapping больше не отклоняет все headers/cookies как S26-pending: effective validated headers и единственная Cookie serialization маппятся в S21T `SecretRequestContext`.
- Cookie может прийти отдельным yt-dlp `cookies` field либо case-insensitive `Cookie` entry внутри `http_headers`. Одна форма принимается; две равные формы deduplicate; competing forms или несколько Cookie headers дают typed `ConflictingCookieMaterial`, без неявного приоритета.
- Malformed header/cookie serialization даёт typed pre-I/O incompatibility. Existing request_data, impersonation, downloader/private extractor state остаются typed candidate rejections.
- Scope для yt-dlp component-а теперь строится от exact target path через `HttpPathScope::from_target_path`, exact normalized origin и HTTPS SecureOnly proof. Redirect policy остаётся `cross_origin_without_secrets`; scope mismatch не раскрывает material.

## Low-level HTTP boundary

- `source-core::HttpRequestScope` — shared origin/path/secure proof, которым одновременно пользуются S21T `SecretRequestScope` и concrete cookie jar; security semantics больше не дублируются между crates.
- `source-core::ScopedHttpCookieJar` — in-memory per-component HTTP source jar без serde/persistence surface. `HttpSourceSession::new_with_cookie_jar` устанавливает его как reqwest cookie provider, поэтому тот же jar обслуживает initial probe, redirects и последующие Range reads.
- Initial yt-dlp Cookie header сохраняется exact, включая duplicate names/order. In-scope `Set-Cookie` обновляет/удаляет только соответствующие cookie names; RFC Domain/Path/Secure/expiry обрабатывает reqwest Jar.
- Внешний `HttpRequestScope` дополнительно gates и чтение, и запись jar-а. Поэтому даже broad `Set-Cookie: Domain=.example` не пересылается на cross-origin/port, sibling path или HTTPS downgrade. Response вне scope не меняет jar.
- Каждый open и active refresh создаёт новый jar из replacement `TransportOpenRequest`; cookies не разделяются между sources и extraction generations.

## Privacy/config/persistence

- Header/cookie values остаются redacted во всех Debug/Display/errors. Concrete request boundary — единственное место раскрытия.
- App credential UI, browser/profile/consent config не добавлены. System yt-dlp по-прежнему сам читает user-owned config/cookies.
- Secret context/jar не имеют serde surface и не входят в playlist state, export, durable reopen payload, config или UI model.

## Focused evidence

- `service-ytdlp`: public/no-auth context, Authorization/Cookie mapping/redaction, same-origin/path/secure proof, conflicting serialization, fresh extraction auth replacement.
- `source-core`: origin/path/downgrade gating, in-scope Set-Cookie, cross-origin rejection, exact duplicate Cookie preservation, redacted Debug.
- `web-media-http`: Set-Cookie used by later Range reads, per-source isolation, Authorization and initial/Set-Cookie cross-origin non-leakage, active refresh uses re-extracted cookie state.
- Existing focused state/export/config tests prove transient request material remains structurally absent and legacy account-session config remains rejected.
- PASS: focused package tests, app-egui 825 tests, strict focused Clippy, Rust 1.92 locked check, cargo-machete, cargo-deny, refactor guardrails, fmt/diff check and Serena diagnostics.
