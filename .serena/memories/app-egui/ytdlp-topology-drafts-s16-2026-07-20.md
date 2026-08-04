# S16 app-owned yt-dlp topology -> ID-less playlist drafts (2026-07-20)

- Новый чистый boundary `app-egui::url_topology_drafts::map_yt_dlp_topology_to_playlist_drafts(&YtDlpMediaLocator, &YtDlpTopology)` преобразует уже service-classified/extracted topology в `YtDlpTopologyDraftPreview` с ordered `PlaylistImportEntryDraft` и bounded typed issues. Queue handle, `PlaylistQueue`, Item/Group allocators, commit transaction, player/runtime I/O и второй URL parser отсутствуют.
- Ownership разделён по владельцам: новый `service-ytdlp::topology::reopen` владеет owner/version/material classification, 8 KiB bound, extractor binary grammar, named `YtDlpDurableReopenIdentityInput` и redacted input/payload/error API; `app-egui/url_topology_drafts.rs` — facade/exact-root admission; `model.rs` — preview/issues/budgets/intent trait; `service_adapter.rs` — zero-allocation borrowed topology adapter; `mapper.rs` — DFS flatten/group mapping, retained budgets и exhaustive service-payload -> neutral locator bridge; focused tests — `url_topology_drafts/tests.rs`.
- Mapping semantics: root `Video` -> Single; Playlist/Collection рекурсивно flatten-ится в source order; `MultiVideo` -> ровно один first-class Compound с ordered Single parts; one retained part остаётся Compound; zero retained parts публикует issues и no draft/IDs. Nested collections внутри compound flatten-ятся в parts; nested MultiVideo внутри MultiVideo не создаёт невозможный nested compound, а flatten-ит собственные children в outer group. Delegation остаётся одним leaf и не запускает повторный resolve/extraction.
- Root Video/Delegation/MultiVideo использует exact caller `YtDlpMediaLocator` через intent-named persistence accessor -> neutral exact `DurableReopenLocator::Url`. Extracted child classification/encoding принадлежит `service-ytdlp` (`YT_DLP_DURABLE_REOPEN_*`): priority StableWebpageIdentity -> StableOriginalIdentity -> binary StableExtractorIdentity `[key_len:u16][key][id_len:u16][id]`. App только исчерпывающе переводит три stable material kinds в `playlist-core`. Это предотвращает accidental direct-media-first reclassification child-а при будущем reopen.
- Format/manifest/fragment/key/signed endpoint/headers/cookies/auth/session material отсутствует в service topology type surface и mapper production source; neutral `DurableReopenLocator` дополнительно fail-closed отвергает ephemeral material. Exact raw locators/payload доступны только intent-named persistence/reopen accessors; Debug/issues не раскрывают secrets.
- Stable unavailable child сохраняется как `PlaylistImportAvailability::Unavailable`; missing identity становится `MissingStableIdentity` issue. Duplicate extractor IDs не dedup-ятся. Issues несут только safe kind + one-based DFS path, capped at 256 с `omitted_issue_count`. Aggregate retained demand capped `MAX_PLAYLIST_ITEMS`; compound также capped `MAX_PLAYLIST_IMPORT_COMPOUND_PARTS` и никогда не публикуется частично.
- Root metadata проецируется в group cached summary (title/duration/video kind), а root exact locator сохраняется в group/part `PlaylistImportProvenance { source_kind: Service }`. Description не переносится, потому что neutral cached metadata/payload boundary не имеет description field; новый обходной service field не добавлялся.
- S16 остаётся отдельным boundary и пока помечен future-consumer allowance: production Add URL orchestration подключит его только в S17. Existing S08 staged import transaction и canonical queue commit не изменены.

## Focused verification

- `cargo test -p service-ytdlp --all-features`: 61 passed, 4 ignored manual; compatibility profile tests also PASS.
- `cargo test -p app-egui url_topology_drafts --all-features`: 9 passed.
- `cargo test -p app-egui --all-features`: 776 passed.
- `cargo +1.96.0 check --workspace --locked --all-features` и `cargo +1.92.0 check --workspace --locked`: PASS.
- Strict `service-ytdlp` Clippy и strict rustdoc `-Dwarnings`: PASS.
- `cargo clippy -p app-egui --all-targets --all-features -- -D warnings -A clippy::large-enum-variant`: PASS. Без точечного allowance strict Clippy останавливается только на двух pre-existing unrelated `large_enum_variant` в `state/strong_media_open{,/pending}.rs`; S16 их не менял.
- `cargo fmt --all --check`, `git diff --check`, `scripts/check-refactor-guardrails.py`, Serena diagnostics: PASS.

## Update 2026-08-04

Service adapter теперь принимает `YtDlpTopologySummary` через `TopologySummaryView`; summary по-прежнему переносит только title/duration, а description явно запрещён production mapping source guard. Актуальная граница rich details/subtitles: `mem:media-services/ytdlp-topology-summary-2026-08-04`.

Related: `mem:playlist/core`, `mem:media-services/ytdlp-topology-s15-2026-07-20`, `mem:media-services/secret-safe-locators-s10b`, `mem:app-egui/ytdlp-playlist-metadata-2026-07-17`, `mem:media-services/ytdlp-topology-summary-2026-08-04`.