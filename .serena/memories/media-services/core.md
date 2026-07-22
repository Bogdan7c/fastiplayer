# Media Services

> **Superseded notice (2026-07-03):** любые упоминания hover preview, hover predecode, hover budget/reservation, timeline-hover prepare или hover overlay ниже являются историческими и не описывают активный контракт. Актуальные owners и запреты: `mem:core` и `mem:frame-server/core`. Остальная non-hover информация этой memory остаётся действующей.

- `media-core` owns neutral `Packet`, `TrackInfo`, `MediaTime`, timeline/snapshot contracts. Packet payload uses `bytes::Bytes`; cloning shares payload ownership.
- `codec-core` owns canonical codec/profile/color stream requirement types and codec adapters. Codec-specific parsing belongs here; VP9 parser is wrapped via `vp9-parser`. Concrete output ownership/transfer belongs to provider-declared `SupportedVideoOutput.frame_contract`, not to `VideoDecodeRequirement`.
- `capability-core::SystemCapabilities::select_best_video_stream()` is the selection gate and returns a selected stream with `matched_output: SupportedVideoOutput`. Use typed `VideoCapabilityRejection` when rejection affects behavior/diagnostics; report code should distinguish unsupported decode format, backend transfer absence, renderer transfer/layout absence, and HDR/P010 policy rejects.
- `symphonia-demux` is the concrete upstream Symphonia demux adapter. `webm-demux` is compatibility re-export for old demux crate path during transition.
- Demux seek returns actual container/decode-safe or approximate position; `player-core` owns final pre-roll/drop/commit semantics.
- `source-core` owns byte access only: local file, HTTP Range, cached byte source, RAM byte-range cache, streaming reader, runtime source config, cancellation/diagnostics. It must not know yt-dlp, containers, codecs, renderer, UI, or player state.
- `media-prefetch` owns neutral prefetch config, pure `PrefetchBufferState` sliding-window RAM state over absolute byte offsets, and `PrefetchingByteSource` with a single background worker thread. The worker takes ownership of the inner `source-core::ByteSource`, starts each open/seek fetch series with `initial_chunk_bytes`, doubles the worker-local read size up to `chunk_bytes`, fills the RAM window without holding the shared mutex during I/O, and foreground reads only from RAM. For in-flight fetch cancellation, `PrefetchSharedState` stores a per-fetch `CancellationToken` under the same mutex as `seek_request`; foreground seek/drop cancel that token, worker passes it to `inner.read`, discards seek-cancelled results as non-fatal, and increments `PrefetchDiagnostics::cancelled_fetches`. Real source errors without seek/shutdown still propagate through `fatal_error`. `PrefetchingByteSource::new` is fallible: `media-prefetch` owns OS thread creation and returns `PrefetchStartupError` with the original `std::io::Error` plus worker-name context before publishing a source; failed spawn drops the captured inner source and cannot leave a partially initialized wrapper. A successfully constructed wrapper still owns cancel/shutdown and synchronous worker join in `Drop`. The crate has no demux, service, player, UI, codec, render, or `rustiplayer-config` dependency; guardrails allow only `source-core`, `tracing`, and `thiserror`. Debug logs (`media_prefetch=debug`) report chunk reads, RAM-window fills, EOF, seek refetch, fetch cancellation, and worker waits.
- `rustiplayer-config::NetworkConfig` drives prefetch policy through `service-ytdlp`: `prefetch_initial_chunk_kb` defaults to 64 KiB, `prefetch_chunk_mb` defaults to 8 MiB and is the slow-start ceiling, and `read_ahead_mb` defaults to 256 MiB and is the prefetch window. Validation requires `prefetch_initial_chunk_kb > 0`, `prefetch_chunk_mb > 0`, `read_ahead_mb > 0`, `prefetch_initial_chunk_kb <= prefetch_chunk_mb * 1024`, and `read_ahead_mb >= prefetch_chunk_mb`; old configs missing prefetch fields deserialize with defaults. `media-prefetch` remains config-agnostic.
- `service-direct-media` owns generic direct `http(s)` media URL opening for seekable `.mp4`/`.mkv`/`.webm` URLs; read `mem:media-services/direct-media` before changing URL policy, direct HTTP opening, or startup routing. It returns `DirectMediaOpenResult` and typed `DirectMediaOpenError`, never `PreparedMedia`, and has no `player-core` dependency.
- `service-ytdlp` owns yt-dlp/yt-dlp details, capability-aware stream candidates/descriptors, direct stream URL/header resolution, compact selected stream identity (`YtDlpSelectedStreamIdentity`), selected-candidate demux opening, refreshable HTTP Range source for VOD, live/non-seekable fallback, and the yt-dlp host allowlist (`youtube.com`, `www.youtube.com`, `m.youtube.com`, `music.youtube.com`, `youtu.be`). Production app startup must use `resolve_yt_dlp_stream_candidates*_with_config` -> `SystemCapabilities::select_best_video_stream()` in the shell/capability layer -> `open_streaming_media_from_candidates_with_demux_config()` for the selected stream id, then store only `YtDlpSelectedStreamIdentity` for app-owned reconstruction/hover. The older `open_streaming_media*` functions are compatibility/manual-test path only and must not be reintroduced as production startup selection. Hover-only yt-dlp VOD open uses `open_seekable_vod_from_selected_identity_with_demux_config()`; unlike playback open, this range-only API must not fallback to streaming for live/non-seekable sources and instead returns typed unsupported.
- `service-ytdlp` wraps seekable yt-dlp VOD `YtDlpRefreshingRangeSource` video/audio sources in `media_prefetch::PrefetchingByteSource` before handing them to `SymphoniaDemuxer`; the flow is `source-core` Range source -> `media-prefetch` RAM read-ahead -> `symphonia-demux`/dual demux -> `player-core`. Live streams and non-seekable Range probe failures stay on `StreamingByteReader`/`spawn_http_fetcher` and are not wrapped in `media-prefetch`. A yt-dlp Range prefetch spawn failure is fatal for that open attempt (no streaming fallback): video/audio context is added through `anyhow::Context` while `PrefetchStartupError` remains downcastable in the source chain.
- `HttpRangeSource` uses caller-provided headers, probes seekability, and has limited retry policy. Since Session 10B it accepts `source_core::SecretHttpUrl`; URL-bearing `SourceError`, tracing, `Debug` and fingerprint are secret-safe, while raw transport identity is exposed only for the actual request. Propagate non-seekable state; do not invent byte-offset seek hints above source/demux boundary.
- Session 10B public direct/yt-dlp resolve/open APIs accept service-owned typed locators, app startup/settings keep those locators without raw `String`, and the service-neutral app classifier table delegates parsing/normalization to each service owner. Domain mapping uses the intentional app → `playlist-core` dependency and the same registry, without a second parser. Full invariants: `mem:media-services/secret-safe-locators-s10b`.
- VP9 codec metadata resolution (`resolver.rs::vp9_requirement_from_codec_tag`): a bare `vp9` tag (which is what yt-dlp reports for yt-dlp SDR ladder formats 242/247/302/303/308/315 etc.) now resolves to a Ready VP9 Profile 0 / 8-bit / 4:2:0 SDR requirement (the canonical yt-dlp SDR/NV12 path). The earlier "no guessing -> Insufficient" policy was fatal once P1-04 routed startup through the capability-aware path. HDR VP9 still arrives only as detailed `vp09.02.*` tags (Ready Profile 2); `reject_hdr_hint_without_resolved_hdr_metadata` re-marks any bare-`vp9`-with-HDR-dynamic_range as Insufficient.
- Temporary policy in `app-egui/startup_media.rs`: `const ALLOW_YOUTUBE_HDR = false` + `yt_dlp_candidate_requires_hdr` filter drop HDR candidates before capability selection, so yt-dlp currently plays SDR only. HDR decode/render is already gated in `capability-core`/`render-wgpu-video`; the const is to become a runtime/UI toggle (no resolver/service change needed when enabled). `VideoDecodeRequirement::requires_hdr_processing()` (codec-core) is the intent query used by the filter.
- yt-dlp cookies/session data must not be stored in TOML config. Future persistent credentials need a separate OS credential/session boundary.
- Settings runtime media reconfigure is app-owned, not service-owned: `app-egui::AppState` records active reconstructible source identity for local/direct/yt-dlp opens, and `frame_prepare::FrameSettingsRuntimeAdapter` rebuilds active direct/yt-dlp sources with current network/yt_dlp config plus capability selection, then restores playback controls from `PlayerSnapshot` where the source can be reconstructed. Service crates still only open/resolve media and must not know settings UI, playback state, or `AppState`.
- Future services should return normalized candidates with source descriptors, headers, codec hints, then let `capability-core` select before demux/player open.

## Session 08B active-source settings rebuild (2026-07-11)

- App composition layer сохраняет `ActiveMediaSource` и для network/demux changes повторно открывает тот же local/direct/yt-dlp source; yt-dlp без codec-policy change сохраняет exact selected stream identity.
- Preferred codec order применяется до capability selection для yt-dlp и как stable video-track ordering для prepared local/direct media.
- Перед active-source rebuild выполняется read-only player lifecycle preflight; seek/scrub/pipeline busy возвращается retryable и не ставит rebuild в скрытую очередь.
- После успешной доставки rebuilt media app восстанавливает volume, selected tracks/quality, current position и play/pause intent.


## Session 10C reusable app media-open mechanism (2026-07-14)
- `app-egui::media_open` reuses existing local/direct/yt-dlp owners without moving service policy into coordinator. Direct uses `DirectMediaOpenResult::media_metadata`; yt-dlp keeps capability selection and exact `YtDlpSelectedStreamIdentity`.
- `ActiveMediaSource` is now the single reconstructible app vocabulary owned by media-open and re-exported through the old state path until Session 10D.
- Local D64/D75 preparation uses one `LocalFileSource` handle and one demux open; `source-core::LocalFileMetadataSnapshot` exposes same-handle size/mtime before transfer. Full invariants: `mem:app-egui/media-open-coordinator-s10c`.


## 2026-07-17 generic yt-dlp URL service (актуальный override)

Этот раздел заменяет более старые утверждения выше о YouTube host allowlist, query normalization и временном HDR constant.

- `service-ytdlp` принимает exact absolute HTTP(S) `YtDlpMediaLocator` для любого host. Locator хранит исходную строку byte-for-byte, включая userinfo/path/query/fragment; разные exact URL являются разными playlist identities. `Debug`/`Display`/UI/tracing/errors показывают только safe host label.
- В app registry порядок неизменяем: `service-direct-media` первым принимает direct media extensions, затем `service-ytdlp` принимает любой оставшийся валидный HTTP(S) URL. Выбранный adapter фиксируется; ошибка direct open не запускает скрытый fallback через yt-dlp.
- `admission.rs` владеет v1 compatibility envelope: один item, без collection/DRM, раздельные direct HTTP(S) WebM VP9 video-only + WebM Opus audio-only streams. Audio-only, muxed-only, HLS/DASH/fragment protocols, missing URL/protocol, unsupported container/codec возвращают typed rejection до transport/demux open.
- `YtDlpServiceError` разделяет invalid locator/scheme, disabled adapter, cancellation, timeout, process plumbing, extractor rejection, invalid response, collection, incompatible streams, transport и demux failures. Raw locator, signed direct URL, headers, selected format identity и stderr не отражаются в диагностике.
- System `yt-dlp` запускается с прежними extraction args и продолжает читать собственные config/cookies; `--ignore-config` и отдельная app auth/session system не добавлены. Старый env selector `VIDEO_PLAYER_YOUTUBE_FORMAT_SELECTOR` сохранён ради compatibility.
- Production цепочка остаётся service resolve -> app/capability selection -> exact selected-stream open -> `PreparedMedia`; `player-core`, decoder и renderer не знают yt-dlp. Seekable Range sources используют existing prefetch path, non-Range direct HTTP может перейти в existing unseekable streaming fallback.
- Playlist metadata enrichment generic для всех `YtDlp` URL и описан в `mem:app-egui/ytdlp-playlist-metadata-2026-07-17`.
- Config/settings contract и migration описаны в `mem:config/schema-v6-ytdlp-migration-2026-07-17`; HDR selection — в `mem:media-services/ytdlp-hdr-selection-s16`.

## S00 yt-dlp 2026.07.04 compatibility inventory (2026-07-20)

- Canonical checked-in owner профиля: `crates/service-ytdlp/compatibility/2026.07.04/`; machine source of truth — `profile.json`, объяснение — `REPORT.md`, optional capture/redaction workflow — `CAPTURES.md`, synthetic corpus — `fixtures/official-synthetic/`.
- Профиль pin-ит official tag/release `2026.07.04`, commit `fdec00e0bf530dc6c3cc7b1dd780e95d9ae460e9`, tree `b14ea6bf92e81a98bdcf652f5e46977c1ee593cc` и observed source archive SHA-256. Он инвентаризирует exact `_format_fields`, result topology, request material, protocol aliases, target/excluded rows и bounded unknown identity (256 UTF-8 bytes).
- Hermetic inventory argv зафиксирован как `--ignore-config --no-plugin-dirs --quiet --no-warnings --simulate --dump-single-json --no-playlist <URL>`; selected variant добавляет `--format <SELECTOR>`. В pinned release plugin isolation flag называется `--no-plugin-dirs`. Runtime production process пока НЕ переключён на hermetic profile: current invocation отдельно зафиксирован как manual opt-in и продолжает читать trusted system/user config/plugins.
- App guarantee относится только к Rustiplayer-owned argv: они не запрашивают download/write/exec/postprocessor/mark-watched behavior. Trusted user config/plugin side effects находятся вне guarantee; user-owned cookie jar может обновляться system yt-dlp.
- `formats` — extractor inventory, `requested_formats` — private-serializable-pinned selected compound components, не inventory. `sanitize_info` превращает неизвестные Python objects в `repr`; generator/WebSocket/private refresh paths классифицированы `RequiresLiveExtractorState` и исключены. `downloader_options` никогда не исполняется.
- Explicit exclusions: RTSP/RTP/MMS, DRM, generators/private live state, arbitrary third-party plugin guarantees. MPEG-PS/AVI/ASF/WMV/WMA/rare codecs в profile v1 — `ProfileExcludedProvisional` из-за отсутствия corpus/runtime evidence, а не вечный запрет.
- Focused contract test: `crates/service-ytdlp/tests/compatibility_profile.rs`; он проверяет exact source/schema coverage, aliases/duplicates, target→session/fixture traceability, raw bounds, formats/requested_formats separation и отсутствие usable URL/header/cookie/key material. S00 не менял production playback code или public/internal runtime API.

## S03 neutral web-media value contracts (2026-07-20)

- Новый workspace crate `web-media-core` — std-only neutral value owner без normal/dev dependencies. Он не знает yt-dlp DTO/process, provider/registry, request/HTTP runtime, config, UI, player, demux или decoder.
- Identity boundary разделяет process-local `SourceIdentity`, immutable `ExtractionGeneration`, snapshot-local `CandidateIdentity` + redacted bounded `CandidateFormatIdentity` и refresh-stable `SemanticIdentity`. Semantic identity включает source lineage; `CandidateDescriptor` typed-ошибкой отвергает cross-source candidate/subtitle rematch.
- Exact raw `protocol`/`ext`/`container`/codec identities сохраняются byte-for-byte до S00 bound 256 UTF-8 bytes и не раскрываются через `Debug`. Рядом хранится parsed transport/container/codec value; unknown остаётся typed `Unknown`, известные S00 exclusions не превращаются в fallback. Container ext/container hints парсятся независимо и дают typed conflict. Codec parser сохраняет exact dot-parameters и различает major web video + proven native audio families, explicit `none`, unknown и неоднозначный bare `mp4a`.
- Layout shape выражена отдельными component types и `StreamLayout::{Muxed, Separate, VideoOnly, AudioOnly}`; невозможные Option-комбинации не публикуются. Normalized video/audio/subtitle descriptors используют named checked `VideoWidth`, `VideoHeight`, rational `FrameRate`, `Bitrate`, `SampleRate`, `ChannelCount` и bounded text/identity values.
- Selection contract: `SelectionRequest::{BestPlayable, Exact}`; `PreferredHeightPolicy` даёт total rank exact → closest lower → closest higher → missing, не захватывая будущие HDR/playability/quality owners. Named width/height upper bound — 16,384 px; его расширение требует отдельного compatibility evidence.
- `StaticCompatibilityRejection`/`ProfileExclusionReason` отделяют profile/metadata incompatibility от будущих operational provider/open errors и сохраняют safe evidence для unknown transport/container/codec cases.
- Focused suite: 14 tests на bounds/redaction, raw preservation, aliases/conflicts, codec family+parameters, четыре layouts, source-safe semantic refresh, numeric bounds и deterministic preferred-height ordering. Rust 1.96 test/strict Clippy/workspace check, MSRV 1.92 check, fmt, refactor guardrails, diff check и Serena diagnostics PASS. `cargo-deny` по-прежнему падает только на известные transitive `quick-xml 0.39.3` RUSTSEC-2026-0194/0195; новый crate зависимостей не добавил. S19 позже владеет mapping public serialized yt-dlp formats в эти values.

## S15 bounded yt-dlp topology extraction (2026-07-20)

- `service-ytdlp` public boundary теперь извлекает owned `Video | Playlist | MultiVideo | Delegation` topology с typed unavailable rows; process/thread/cancellation state не выходит из service.
- Exact topology argv сочетает official `--dump-json` lazy child lines с финальным authoritative `--dump-single-json`, плюс `--flat-playlist --lazy-playlist`; `n_entries` игнорируется. Production продолжает читать trusted system config/cookies/plugins, hermetic profile добавляет config/plugin isolation.
- Stdout/stderr/line/entry/JSON-depth/topology-depth/field budgets, kill+wait cancel/timeout/overflow, redaction, missing `_type`, multi-video root validation и разные `url`/`url_transparent` merge policies закреплены focused tests.
- Полный API/argv/ownership/test contract: `mem:media-services/ytdlp-topology-s15-2026-07-20`.

## S15A approved top-level input-scheme admission (2026-07-20)

- `service-ytdlp::YtDlpInputScheme` — typed pure input vocabulary: exact `http`, `https`, `ftp`, `ftps`, `rtmp`, `rtmpe`. Locator parser сохраняет исходную строку byte-for-byte и не нормализует wire variants. `rtmps`/`rtmpt`/`rtmpte`, `rtmp_ffmpeg`, file/RTSP/RTP/MMS и unknown schemes typed rejected; `rtmp_ffmpeg` является invalid URI syntax, а не RTMP alias.
- Input parsing не обещает transport availability. `app-egui::url_service_adapter` владеет единственным direct-first registry: HTTP(S) direct-media остаётся первым, любой remaining HTTP(S) остаётся yt-dlp fallback; extended scheme admitted только exact registration row типа `ImplementedYtDlpInputProviderCapability`.
- Production extended capability list пуст, потому что S37/S39 providers ещё не реализованы. Поэтому FTP(S)/RTMP parser rows dormant и возвращают typed `ImplementedProviderUnavailable`; test registry доказывает absent/active и exact per-scheme registration без alias expansion.
- После выбора adapter-а registry больше не участвует: direct request не содержит yt-dlp fallback path, поэтому direct open failure не запускает второй adapter.
- Query/userinfo locator-ы используют общий aggregated durable-locator acknowledgement. Sensitive yt-dlp append continuation сохраняет metadata source/config через confirmation и запускает enrichment только после matching commit.
- Focused owners/tests: `crates/service-ytdlp/src/locator.rs`, `crates/app-egui/src/url_service_adapter.rs`, `crates/app-egui/src/url_service_adapter/tests.rs`, confirmation handoff в `crates/app-egui/src/playlist_runtime/actions.rs`.

## S16 durable topology identity + app draft mapping (2026-07-20)

- `service-ytdlp::topology::reopen` — canonical owner stable child reopen classification: `YtDlpDurableReopenPayload`, `YtDlpDurableReopenMaterialKind`, safe typed errors, owner/version/8 KiB constants и exact classifiers для topology identity/delegation target. Priority: webpage -> original -> extractor key+ID; v1 extractor grammar `[key_len:u16][key][id_len:u16][id]`. Raw payload раскрывается только intent-named persistence accessors, Debug показывает category+byte count.
- App `url_topology_drafts` не кодирует service schema: он исчерпывающе переводит только три service stable material kinds в neutral `playlist-core::DurableReopenLocator`, а missing/oversized classification превращает в bounded safe issue. Exact root locator остаётся exact URL; extracted child остаётся service-owned, поэтому future reopen не попадает под accidental direct-media-first reclassification.
- Video/Collection/MultiVideo/Delegation -> ID-less Single/Compound mapping, bounds/tests и S17 handoff: `mem:app-egui/ytdlp-topology-drafts-s16-2026-07-20`.


## S18 playlist/topology hardening gate (2026-07-21)
- Existing yt-dlp Video/Playlist/MultiVideo/Delegation extraction, durable reopen mapping, URL Append-only and redaction suites вошли в milestone PASS; production service API/process logic не менялись.
- `web-media-core` теперь required std-only neutral contract в dependency guardrails и blocking coverage inventory. Полный handoff: `mem:playlist/topology-hardening-s18-2026-07-21`.

## S19 generic yt-dlp candidate normalization (2026-07-21)

- `service-ytdlp` now maps public serialized yt-dlp formats into neutral `web-media-core` values through a versioned, secret-safe candidate snapshot API. Inventory rows stay one-to-one visible accepted/rejected entries; selected results use one ordinary component or exact validated compound components without Cartesian expansion.
- Exact selection is snapshot/generation-bound; cross-extraction recovery is semantic and layout-checked, with typed stale/ambiguous failures. Provider request material stays in schema v1 and rejects non-reconstructible/private requirements visibly.
- Full ownership, identity, request-material, HLS compatibility, limitations and verification contract: `mem:media-services/ytdlp-candidate-normalization-s19-2026-07-21`.


## S21/S21R demux registration и event-first read boundary (2026-07-21)

Neutral typed demux registry/composition принадлежит `demux-api`; подробные contracts и tests см. `mem:demux-api/core`. `media_core::Demuxer` теперь read-only через required `next_event`; generic `next_packet` удалён. `DemuxReadEvent::TemporarilyUnavailable(DemuxRetryHint)` является отдельной nonterminal readiness identity, не EOF/error/track mutation, а finite packet mapping централизован в `media_core::finite_packet_read_event`. Media services/composition roots должны передавать typed input capability/hints/budget и concrete factories, не встраивая probe/container policy обратно в service owner. Readiness-enabled staged install и generation-fenced player retry scheduling остаются S21W; current services до него не публикуют temporary readiness в staged preflight.

## S21T neutral web transport boundary (2026-07-21)

- Новый `web-media-transport-api` создаёт provider registry/open/refresh boundary до первого provider-а и зависит только от `web-media-core`, `source-core`, `thiserror`. Exact+semantic component identity, runtime source generation, VOD/live, seekable/cancellation-aware streaming, typed unavailable/unsupported/auth/transport/refresh outcomes и scoped request material не знают yt-dlp DTO, demux/player/queue/UI.
- `source-core` владеет exact-secret `HttpRequestTarget`, normalized origin/path policy evidence, validated headers и `StreamingByteSource` с cancellation token на каждом read. `SecretRequestContext` покрывает headers/cookies/request_data/segment+key overrides и fail-closed проверяет origin + segment-boundary path + secure scope; cross-host redirect secrets не получает.
- Первый concrete `web-media-http`, direct-media adapter migration и yt-dlp material mapping остаются S21U/S26. Полный API/test/limitation contract: `mem:media-services/web-transport-s21t-2026-07-21`.


## S21C neutral playback planner (2026-07-21)
- До I/O playable layout выбирает pure crate `web-media-playback-plan`; composition передаёт immutable transport, demux, video/system и S20 audio capability snapshots.
- `SelectionRequest::Exact` теперь использует source-safe `ExactSelectionIdentity` (exact + semantic identity); operational open failures не относятся к planner rejection.
- Подробности и проверки: `mem:media-services/web-playback-planner-s21c-2026-07-21`.


## S22 concrete progressive HTTP provider (2026-07-22)

- `web-media-http` is the only new concrete provider and depends normally only on `source-core`, `media-prefetch`, and S21T `web-media-transport-api`; dependency guardrails enforce this.
- `source-core::HttpSourceSession` owns the single manual-redirect reqwest client and reuses the first Range probe response/client for both Range and non-Range outcomes.
- `service-direct-media` consumes it through the neutral contract. `service-ytdlp` remains extractor/descriptor owner without a concrete HTTP dependency.
- Full details and focused proofs: `mem:media-services/progressive-http-s22-2026-07-22`.


## S23 queue-owned yt-dlp open (2026-07-22)

- Current playback integration supersedes historical service-owned WebM opener notes: `service-ytdlp` stops at extraction/topology/locator/metadata plus neutral S19 planning/S21T request mapping; app composition owns S21C selection and S22 HTTP/demux registries.
- Legacy `YtDlpStreamingMedia`, selected-stream DTOs and direct reqwest/prefetch/Symphonia opener modules are deleted. Exact reconstruction uses `YtDlpCandidateSelection` with fresh-generation semantic rematch.
- Current contract and S26 limitation: `mem:app-egui/queue-owned-web-open-s23-2026-07-22`.
