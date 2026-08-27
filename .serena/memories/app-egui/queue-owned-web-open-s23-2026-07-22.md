# S23 — Queue-owned web open integration (2026-07-22)

> S36 extension (2026-07-25): exact muxed ISM/MSS fMP4 H.264+AAC static VOD now composes through `web_media_open::smooth`. App owns planner registration, bounded policy, one injected S28A/F3A registry, fresh C3 catalog generation/finalization and neutral receipted seek projection; `web-media-smooth` owns manifest/sources/demux transaction. The same Ready → authorize → Installed barrier remains authoritative. Full contract: `mem:media-services/smooth-vod-runtime-s36p4-p6-2026-07-25`.

> S34/S35 extension (2026-07-24): static and dynamic DASH material is now owned by `web-media-dash` and app composition through the same queue/barrier architecture. Therefore older statements below that all DASH/non-progressive material is globally fail-closed are historical; only material without an implemented exact provider remains rejected. Full contracts: `mem:media-services/dash-vod-s34-2026-07-24` and `mem:media-services/dash-live-s35-2026-07-24`.

> S33 extension (2026-07-24): explicit public yt-dlp live intent composes `web-media-hls` live runtime plus an app-owned bounded endpoint-refresh/rematch port. Normal/startup/settings preparation installs the neutral S31L port before the same Ready → authorize → enqueue → Installed barrier and never publishes service finite duration for live. Queue/current ownership is unchanged. Full contract: `mem:media-services/hls-live-s33-2026-07-24`.

> S32C extension (2026-07-23): `web_media_hls_open` composes exact yt-dlp HLS material, concrete TS/fMP4 registry, strict master/alternate-audio evidence and subtitle descriptors through the same Ready → authorize → enqueue → Installed barrier. HLS decode-safe seek and atomic separate A/V replacement are documented in `mem:media-services/hls-vod-s32c-2026-07-23`.

Связанные memories: `mem:core`, `mem:app-egui/media-open-coordinator-s10c`, `mem:app-egui/startup-orchestration-s17`, `mem:media-services/progressive-http-s22-2026-07-22`, `mem:media-services/web-playback-planner-s21c-2026-07-21`, `mem:media-services/web-transport-s21t-2026-07-21`, `mem:player-core/core`.

## Ownership и production flow

- `PlaylistRuntime` по-прежнему единолично владеет exact Item ID, queue revision, reservation/commit barrier, manual/automatic/compound navigation, remove/tombstone и current/active binding. Ни `service-ytdlp`, ни `web_media_open` не получили queue vocabulary или commit authority.
- Новый app composition module `crates/app-egui/src/web_media_open.rs` — единственный yt-dlp playback composition path: S19 `YtDlpCandidateSnapshot` -> S21C `plan_playback` -> S22 `TransportRegistry`/`WebMediaHttpProvider` -> `DemuxRegistry`/`SymphoniaDemuxFactory` -> single demuxer либо neutral `CompositeAvDemuxer` для separate A/V.
- `BestPlayable` используется для нового Play и явной settings reselect policy. `Exact(YtDlpCandidateSelection)` используется для suspend/resume/settings rebuild: выполняется fresh extraction с той же `SourceIdentity`, следующей `ExtractionGeneration` и semantic rematch; ambiguous/missing/changed candidate fail-closed до barrier.
- `PreparedYtDlpWebMedia` содержит demuxer, exact installed candidate token и `YtDlpPlaylistMetadata` из того же extraction snapshot. Demux metadata остаётся primary; service title/duration только заполняют отсутствующие значения.
- `ActiveMediaSource::YtDlpUrl` хранит locator и boxed exact candidate selection. Source становится current/active только через прежний Ready -> explicit authorize -> EnqueuedAtPlayerOwner -> exact Installed protocol.
- CLI, restored current, MPRIS/manual/automatic queue transport, compound part Play, suspend/resume и settings проходят через существующие typed locator/active-source boundaries. URL классификация остаётся в одном `StartupUrlServiceRegistry`; direct-media имеет приоритет, выбранный adapter не меняется после open failure.

## Service boundaries и удалённый legacy path

- `service-ytdlp` теперь владеет extraction/topology/locator/metadata и преобразованием S19 candidate в S21C planning snapshot + neutral S21T `TransportOpenRequest`. Он не зависит от `reqwest`, `web-media-http`, `media-prefetch`, `symphonia-demux` или player/app crates.
- Старые public WebM-only opener/selection DTO и implementation удалены: `admission.rs`, `selection.rs`, `resolver.rs`, `http_refresh.rs`, `http_stream.rs`, `YtDlpStreamingMedia`, `YtDlpSelectedStreamIdentity` и `open_*media_from*`. Временного forwarding adapter нет.
- Selected result и inventory остаются раздельными; accepted iteration ставит selected result первым, чтобы duplicate exact ID не потерял richer request material. Planning и transport используют один exact `CandidateIdentity`.
- S19 snapshot теперь содержит title/duration того же `--dump-single-json` generation; второй metadata extractor process не запускается.
- S26 снял прежнее `AuthorizationMappingPending` limitation: progressive effective headers/cookies маппятся в scoped `SecretRequestContext`, а concrete HTTP session использует per-source ephemeral Set-Cookie jar. Request material без exact implemented owner по-прежнему fail-closed; HLS и DASH уже имеют более новые concrete owners, а ISM получает отдельную S36 projection/provider цепочку. Полный auth boundary: `mem:media-services/ytdlp-system-auth-s26-2026-07-22`.

## Lifecycle и cancellation

- Recoverable extraction/planning/transport/demux failure остаётся до authorization barrier и сохраняет старое playback. После `EnqueuedAtPlayerOwner` прежний coordinator обязан закончить exact Installed/fatal path.
- `PreparationCancellation` теперь владеет cloneable `source_core::CancellationToken`; cancel/supersede/suspend/shutdown одновременно сохраняет typed player cancellation cause и прерывает S22 transport/progressive demux.
- Candidate resolver использует существующий cancellable yt-dlp process primitive, поэтому running extractor также останавливается. CLI startup job передаёт и atomic cancellation callback, и тот же source token; shutdown отменяет их до bounded join.

## Focused proof и проверки

- `service-ytdlp`: exact/semantic rematch, selected compound, muxed/separate shapes, planning/transport exact parity, snapshot metadata generation, S26 auth preservation, cancellation, redaction и source-level guard against legacy public opener/dependencies.
- `app-egui`: coordinator barrier/cancel winner, exact revision/intent, local/direct prepared parity, compound part Installed-only current publication, stale/remove/tombstone, automatic compound traversal, restore/settings compensation, startup shutdown token propagation.
- S22 neighbor tests покрывают Range/non-Range, MP4/M4A/WebM, separate A/V composition, generation fences и planner exact/stale semantics.
- Проверено: `scripts/ci-checks.sh tests` (workspace PASS), `cargo test -p app-egui` (805 PASS), `cargo test -p service-ytdlp` (48 PASS), S22 focused packages PASS, `cargo clippy -p service-ytdlp --all-targets -- -D warnings`, `cargo machete --with-metadata crates/service-ytdlp crates/app-egui`, `scripts/check-refactor-guardrails.py`, fmt/diff-check/reference audit и Serena diagnostics. App Clippy остаётся с двумя прежними untouched `large_enum_variant` warnings в `state/strong_media_open.rs` и `state/strong_media_open/pending.rs`.


## S42 executor wave 5 — private concrete runtime owner (2026-08-27)

- `crates/app-egui/src/web_media_open/runtime.rs` теперь владеет `WebOpenRuntime`: construction concrete transport/demux registries, immutable capability snapshots, candidate physical open, demux open/readiness wrapping и private config-to-budget helpers.
- `web_media_open.rs` сохраняет top-level yt-dlp extraction/rematch, source/timeline/catalog generation, cancellation fences до open и перед publication, exact candidate/component identity, separate A/V composition, stream/catalog finalization и strong-install-facing prepared envelope.
- HLS/DASH refresh ports и timeline generation по-прежнему строятся parent attempt owner-ом и передаются в runtime через typed `WebCandidateOpenContext`; runtime не получил queue/Installed authority. Parent/child production line counts: `686/538`, оба ниже 800.
- Focused `web_media_open::` suite: 46/46 PASS. Full app no-default и all-features: 1002/1002 в каждой matrix; strict Clippy обеих matrix, fmt/diff/refactor guardrails, S41 cross-provider integration 3/3, S42 final acceptance 24/24 и Serena diagnostics PASS.
- Historical S41 `runtime-coverage-s41.json` остаётся immutable. `cross_provider_integration_s41.rs` exact-map-ит только пару `(web_media_open.rs, fn open_candidate)` в canonical `web_media_open/runtime.rs`; соседний `prepare_yt_dlp_web_media` по-прежнему проверяется в parent. Это предотвращает path-wide redirect, который мог бы скрыть stale evidence.
