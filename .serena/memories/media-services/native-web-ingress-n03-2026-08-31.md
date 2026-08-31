# N03 typed extractor adapter и injected process spy (2026-08-31)

## Ownership и public boundary

- `web-media-core::ExtractorInvocationReason` остаётся единственным product-reason vocabulary; service второй enum не создаёт.
- `service-ytdlp::YtDlpExtractorAdapter` — instance-owned façade candidate/topology/metadata extraction. Caller обязан передать typed reason; старые public free functions сохранены как compatibility wrappers с `PageMediaResolution` либо `CollectionTopologyResolution`.
- `ExtractorProcessLauncher::spawn(&mut Command, ExtractorProcessInvocation)` является единым dependency-injected spawn boundary. `ExtractorProcessInvocation` содержит только reason и `ExtractorProcessPhase::{CandidatePrimary,TopologyPrimary,RecoveryPageCapture,RecoveryEmbedCandidate}`; URL/argv/request material в event отсутствуют.
- Production `Default` создаёт локальный `Arc<SystemExtractorProcessLauncher>`; global mutable singleton/test hook отсутствует. Hermetic tests подменяют только per-Command PATH и запускают тот же переданный `Command`, поэтому Unix process-group pre-exec не теряется.

## Process lifecycle invariant

- `process_tree::spawn_owned_process_with_launcher` конфигурирует отдельную process group до launcher call и затем немедленно передаёт полученный `Child` прежнему `OwnedProcess`.
- ETXTBSY bounded retry, общий operation deadline, cancellation checks, stdout/stderr/JSON budgets, finish-before-reader-join, group kill/reap и Drop insurance не менялись.
- Все production OS child spawn paths проходят launcher: candidate/metadata primary, topology primary, HTML write-pages recovery и каждый recovered embed candidate. Recovery phases сохраняют исходный product reason.
- Реальные app callsites маркируют initial page как `PageMediaResolution`, playlist import/metadata discovery как `CollectionTopologyResolution`, exact/composed fresh rematch и HLS/DASH endpoint refresh как `ExtractorBackedRecovery`.

## Узкая app projection

- Новый `app-egui::web_media_extractor_adapter` не меняет `ActiveMediaSource` и не создаёт N04 envelope.
- `ExtractorCatalogProjection` переиспользует canonical `YtDlpCandidateSnapshot::planning_projection()` как существующий neutral `PlanningCandidateSnapshot`, сохраняет row-local rejection count и metadata того же generation, и fail-closed проецирует official live intent в `WebMediaPresentationKind::{Vod,Live}`.
- После runtime selection projection создаёт N01 `WebMediaSelection`. После component catalog installation она переиспользует canonical existing `ComponentVariantSelection`; parent-only и exact-components формы не создают второй catalog/identity vocabulary.
- Metadata остаётся существующим `YtDlpPlaylistMetadata` только внутри узкого N03 bridge; полный prepared/source envelope и удаление extractor DTO из app lifecycle принадлежат N04+.

## Functional evidence и проверки

- Hermetic public-adapter tests: YouTube-like formats+metadata+exact page reason; HTML platform-hijack recovery с phases primary/write-pages/embed и title/formats parity; topology exact collection reason; cancellation реального recovery descendant с bounded group cleanup и recovery reason.
- App tests: public extractor snapshot доходит до neutral catalog/selection/presentation/metadata; production direct-media MP4 classification оставляет injected extractor spy пустым; VOD/live fail-closed mapping; component model подтверждает parent-only и exact-components neutral shapes.
- PASS: `cargo test -p service-ytdlp --lib --locked` (154/154), N03 app projection 3/3, component variants 9/9, content-probe fallback 11/11, strict Clippy для `service-ytdlp` + `app-egui`, fmt, diff-check, workspace all-target/all-feature locked check и Serena diagnostics.
- Полный `cargo test -p service-ytdlp --locked` дошёл до S42 evidence catalog: все unit и предшествующие integration suites green, но один existing evidence test требует отсутствующий вне N03 файл `user/web_media_url_playback_implementation_plan.md`. N03 не подменяет этот historical evidence.
- N04 не начат; push не выполнялся.

Related: `mem:core`, `mem:media-services/native-web-ingress-n01-2026-08-31`, `mem:media-services/ytdlp-process-owner-2026-08-05`, `mem:media-services/ytdlp-candidate-normalization-s19-2026-07-21`, `mem:media-services/generic-site-open-2026-07-27`.