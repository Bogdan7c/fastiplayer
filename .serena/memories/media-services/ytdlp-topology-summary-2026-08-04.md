# yt-dlp topology compact summary boundary (2026-08-04)

## Причина
- Реальная ссылка `https://www.youtube.com/watch?v=d4PzXZnLZPI` возвращает description размером 5019 UTF-8 bytes. Старый topology parser применял общий 4 KiB metadata budget и фатально отвергал весь playable video как `FieldBudgetExceeded`, хотя app mapper description не использовал.
- Причина устранена на boundary, а не повышением лимита: rich editorial metadata не является структурой media URL и не должна блокировать topology/import.

## Service-owned contract
- Public `YtDlpTopologyMetadata` заменён compact `YtDlpTopologySummary`; nodes expose `summary()`, delegation — `wrapper_summary()` / `merge_resolved_summary()`.
- Summary владеет только bounded `title` и finite non-negative `duration`. Description вообще не читается topology parser-ом. Будущие длинные описания должны появиться через отдельный lazy rich-details API, а не расширять topology.
- Missing/available/malformed различаются публичным `YtDlpTopologySummaryFieldState`; safe причины — `UnexpectedType`, `EmptyValue`, `FieldBudgetExceeded`, `InvalidNumericValue`.
- Optional summary field никогда не делает structurally playable node fatal. Fatal остаются structure/identity/locator/entries/budget invariants. Video требует extractor identity и playable direct URL/formats, но не title.
- Delegation policy переименован в `YtDlpDelegationSummaryPolicy`. Transparent wrapper переопределяет resolved поле только пригодным wrapper value; если wrapper Missing/Unavailable без пригодного fallback, typed resolved/wrapper failure не теряется.

## App boundary и будущее
- `app-egui::url_topology_drafts` адаптирует только borrowed `TopologySummaryView { title, duration }` в neutral cached playlist metadata. Production source guard запрещает `description`, transport material и второй URL parser.
- Playlist topology/queue остаётся компактной структурой. Rich descriptions — отдельный lazy details/enrichment boundary. Subtitles — отдельный ancillary-track catalog/runtime; durable queue хранит только пользовательский selection intent, а не полный subtitle payload.
- Изменение public Rust API intentional; compatibility aliases старых metadata names не оставлялись, чтобы не закреплять неверную архитектурную семантику.

## Verification
- Live service extraction указанной YouTube-ссылки: PASS, `YtDlpTopology::Video`.
- `cargo check -p service-ytdlp -p app-egui`: PASS.
- `cargo test -p service-ytdlp --lib`: 107 PASS.
- `cargo test -p service-ytdlp --lib topology::parser`: 10 PASS.
- `cargo test -p app-egui url_topology_drafts`: 9 PASS.
- Targeted all-target/all-feature Clippy with allowance only for pre-existing unrelated `collapsible_if`: PASS.
- `git diff --check` and Serena diagnostics for changed core files: PASS.

Related: `mem:media-services/ytdlp-topology-s15-2026-07-20`, `mem:app-egui/ytdlp-topology-drafts-s16-2026-07-20`, `mem:app-egui/url-collection-import-s17-2026-07-20`, `mem:media-services/core`.