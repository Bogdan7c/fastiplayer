# N13B extractor-spawn, redaction и persistence ratchet (2026-09-01)

## Root cause и boundary fix

- Audit подтвердил ложноположительный process-spy proof в N06–N13A verticals: tests создавали `YtDlpExtractorAdapter::with_process_launcher(spy)`, но production `prepare_source` строил отдельный `YtDlpExtractorAdapter::default()`. Поэтому assertion `0` не наблюдал реальный fallback subprocess boundary.
- `media_open::WebMediaOpenSettings` теперь несёт cloneable `extractor_adapter` одного open/reopen attempt-а. Native HLS/DASH/Smooth/HDS fallback reconstruction сохраняет тот же adapter, а `prepare_yt_dlp_web_media` и semantic exact/composed rematch используют только переданный owner.
- Extractor-backed HLS/DASH endpoint refresh ports также сохраняют clone того же adapter; recovery больше не создаёт скрытый `Default` launcher. Startup/import/metadata entrypoints по-прежнему создают собственный instance-owned adapter ровно на один самостоятельный operation.
- Public API и protocol/data plane не менялись. Изменение локализовано в app internal composition boundary; cancellation, process-group ownership, budgets, typed reasons и recovery semantics остались прежними.

## Exact process-spawn allowlist

Production `service-ytdlp` содержит ровно один непосредственный zero-argument process spawn:
- `crates/service-ytdlp/src/invocation.rs` — `SystemExtractorProcessLauncher::spawn -> Command::spawn`.

`spawn_owned_process_with_launcher` встречается ровно в трёх production sources:
- `crates/service-ytdlp/src/process.rs` — candidate/metadata process caller;
- `crates/service-ytdlp/src/topology/process.rs` — topology process caller;
- `crates/service-ytdlp/src/process_tree.rs` — единственная definition/lifecycle owner boundary.

Source ratchet `invocation::tests::production_process_spawn_entrypoints_match_exact_injected_owner_allowlist` фиксирует оба списка и counts. `OwnedProcess` остаётся единственным Child/process-group/reap owner-ом.

## Exact app provider-DTO allowlist

Production service-ytdlp provider DTO markers разрешены только в:
- `playlist_runtime/url_import.rs`;
- `startup_media/yt_dlp.rs`;
- `url_topology_drafts.rs`, `url_topology_drafts/mapper.rs`, `url_topology_drafts/model.rs`, `url_topology_drafts/service_adapter.rs`;
- `web_media_dash_open.rs`, `web_media_dash_refresh.rs`;
- `web_media_extractor_adapter.rs`;
- `web_media_hls_open.rs`, `web_media_hls_refresh.rs`;
- `web_media_open.rs`;
- `web_media_open/catalog.rs`, `component_variants.rs`, `content_probe_fallback.rs`, `hds.rs`, `preparation.rs`, `runtime.rs`, `smooth.rs`, `source_state.rs`.

Ratchet `web_media_extractor_adapter::tests::provider_dtos_stay_inside_exact_extractor_adapter_allowlist` рекурсивно проверяет production sources. Queue/session/UI/persistence не входят в allowlist. Отдельный field-shape ratchet разрешает active extractor state только stable selection identity и запрещает normalized candidate, transport contexts, DASH fragments, request/header/cookie/SecretHttpUrl types.

## Functional/security/request proofs

- Public page fixtures YouTube-like и HTML/W3Schools-like выполняют ровно один `CandidatePrimary` spawn каждая с exact `PageMediaResolution`. HTML fixture несёт ephemeral endpoint query, Authorization и cookie sentinels; snapshot Debug их не раскрывает. Existing recovery fixture сохраняет исходный typed reason во всех primary/write-pages/embed phases.
- Native HLS TS+fMP4, HLS live, DASH H.264/AAC+VP9/Opus, DASH live, Smooth и HDS verticals теперь включают extractor policy и устанавливают fail-fast injected spy в настоящий production attempt. Open/seek/switch/endpoint recovery/semantic reopen дают spy count 0.
- Direct HTTP Ogg и FTP Ogg остаются structural no-extractor owners, работают при `yt_dlp.enabled=false`, достигают nonzero PCM, seek/reopen и recovery attachment. HTTP WebM достигает VP9 decode -> WGPU submit/release.
- Для HTTP/WebM, HLS, DASH, Smooth и HDS request accounting теперь отдельно фиксирует root count 0 до open; после open/switch/recovery/reopen сохраняются exact existing counts. Это запрещает classifier fetch и duplicate root handoff.
- Playlist-state writer output не содержит transient `format_url/manifest_url/fragment_url/key_url/signed_endpoint/query_payload/headers/cookies/authorization` fields; strict reader отвергает и forbidden material kinds, и injected unknown fields, а diagnostic Debug не отражает payload sentinel.
- Active source Debug redaction и direct/native neutral projections остаются зелёными; active source shape не может хранить endpoint/header/cookie transport types.

## Verification

PASS:
- `cargo test -p service-ytdlp --lib --locked invocation::tests` — 6/6;
- `cargo test -p app-egui --all-features --locked media_open::web::tests::native_` — 10/10;
- `cargo test -p app-egui --all-features --locked web_media_open::content_probe_tests::direct_progressive` — 4/4;
- `cargo test -p app-egui --all-features --locked web_media_extractor_adapter::tests` — 5/5;
- focused active-source Debug redaction — 1/1;
- `cargo test -p playlist-state --locked` — 53/53;
- strict Clippy for service-ytdlp/app-egui/playlist-state;
- workspace all-target/all-feature locked check;
- fmt, diff-check и Serena diagnostics.

По §6.3 full workspace tests, MSRV, dependency/release/coverage gates и public/hardware cases не запускались; они принадлежат G2. Следующая session — G2, N13B implementation не продолжать.

Related: `mem:core`, `mem:media-services/native-web-ingress-n03-2026-08-31`, `mem:media-services/native-web-ingress-n13a-2026-09-01`, `mem:media-services/ytdlp-process-owner-2026-08-05`, `mem:media-services/secret-safe-locators-s10b`.
