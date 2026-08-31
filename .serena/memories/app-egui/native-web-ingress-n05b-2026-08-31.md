# N05B — provider-neutral same-item/reopen lifecycle (2026-08-31)

## Граница и владельцы

- `WebMediaSourceIntent` остаётся единым app-owned web source envelope. UI, same-item lifecycle, settings, recovery и queue/reopen consumers зависят только от neutral intent methods и не сопоставляют concrete ingress provider.
- `ExtractorMediaSourceState` в `web_media_open/source_state.rs` владеет exact `YtDlpCandidateSelection`, optional composed selection и reverse catalog routes. Эти provider DTO не выходят из extractor adapter modules.
- `WebMediaStreamConfiguration` хранит только N01 `WebMediaSelection`, safe presentations, generation/preference и component projections. Удалён `web_media_stream_model/catalog_routes.rs`; reverse neutral-target → provider-token route принадлежит `web_media_open/catalog.rs`.
- `WebMediaSourceIntent::selection_switch_request` и `settings_reconfigure_request` — source-owned intent boundaries. Caller передаёт named neutral policy/action, а source owner возвращает `NoChange | Ready/Reopen | Unsupported | Stale` или готовый `WebMediaOpenRequest`.
- Settings rebuild использует единый `media_open::prepare_source_synchronously` path вместо трёх копий direct/native/extractor composition.
- Удалены временные `ExtractorSourceBridge`, `WebMediaSourceAdapterBridge`, `into_adapter_bridge`, `extractor_bridge` и старые app web variants. `MediaPreparationFailureKind::ExtractorOpen` и `PreparedStartupMedia::Extractor` больше не публикуют provider name на generic boundary.

## Инварианты

- Active/InstalledOnly action inert; direct settings no-op проверяется до runtime busy preflight.
- Pending same-item transaction single-flight и блокирует conflicting action без изменения playback.
- Playing/Paused, exact receipted VOD position, playlist item identity и source lineage сохраняются через Installed; только media instance/generation заменяются.
- Pre-barrier failure оставляет установленный playback, preference, item/lineage и visible generation прежними, снимая pending и публикуя bounded safe error.
- Catalog switch требует current generation; stale target не получает provider reopen token.
- Suspend/reopen сохраняет installed exact parent/component semantics; queue и persistence продолжают владеть своими neutral item/lineage/receipt identities.
- Old `YtDlpNormalizedCandidate|CandidateSelection|ComposedSelection|LiveIntent|CandidateSnapshot` разрешены только в extractor/open/refresh adapter modules.

## Functional evidence

- Same-item lifecycle: 4/4 (Playing, Paused, pre-barrier failure, pending conflict).
- Stream model: 26/26 (stale generation, active no-op/controller, selection/presentation secrecy).
- Media-open: 113/113; extractor-open: 47/47. Production fixtures достигли ненулевого PCM для HTTP Ogg Opus, scoped-cookie Ogg, FTP Ogg Vorbis и forward-seek Vorbis.
- Settings runtime 54/54; suspend/resume 12/12; resume persistence 9/9.
- `cargo fmt --all -- --check`, `git diff --check`, strict `app-egui` all-target/all-feature Clippy и workspace all-target/all-feature check PASS.
- Full workspace tests, MSRV, dependency gates, release, coverage, GUI/public media/hardware deliberately NOT RUN до G1.

Связанные знания: `mem:core`, `mem:app-egui/native-web-ingress-n05a-2026-08-31`, `mem:media-services/native-web-ingress-n04-2026-08-31`, same-item switch, queue/reopen, settings-runtime и media-open lifecycle memories.