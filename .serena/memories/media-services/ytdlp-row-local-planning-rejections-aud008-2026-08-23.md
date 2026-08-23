# AUD-008: row-local yt-dlp planning rejections (2026-08-23)

## Подтверждённая причина

- Независимая read-only сессия прогнала публичный `resolve_yt_dlp_candidate_snapshot_with_config` с герметичным двухстрочным yt-dlp fixture: progressive HTTPS/MP4 H.264 `avc1.640028 + mp4a.40.2` и соседний bare `hevc + mp4a.40.2`.
- Production normalizer принял обе rows (`inventory=2, accepted=2`), но прежний `YtDlpCandidateSnapshot::planning_snapshot` использовал fail-fast `collect<Result<Vec<_>>>`; bare HEVC дал `RuntimeRequirement`, весь вызов вернул `Err`, уже построенный H.264 стал недоступен.
- Причина была в aggregation boundary между service-owned normalization и neutral planning, а не в HEVC parser-е: bare H.265 family распознана честно, но без полного codec profile runtime requirement её нельзя планировать.

## Новый ownership/API contract

- `service-ytdlp::YtDlpPlanningProjection` владеет двумя согласованными результатами одного canonical traversal:
  - `snapshot(): &PlanningCandidateSnapshot` — только statically-compatible rows;
  - `rejections(): &[YtDlpPlanningCandidateRejection]` — row-local отказы в traversal order.
- Каждый `YtDlpPlanningCandidateRejection` сохраняет exact `CandidateIdentity` и `YtDlpPlanningCandidateRejectionReason` (`RuntimeRequirement` либо typed `PlanningCandidateBuildError`).
- `YtDlpCandidateSnapshot::planning_projection()` — полный диагностический boundary. `planning_snapshot()` сохранён как совместимый convenience API и извлекает neutral snapshot из projection; одна непланируемая row больше не делает весь snapshot ошибкой.
- `app-egui::prepare_yt_dlp_web_media` использует полный projection, передаёт neutral snapshot существующему planner/catalog/stream-model коду и включает безопасный `planning_rejections` count в failure context.
- Neutral `web-media-playback-plan` не знает provider-specific rejection types; service-ytdlp остаётся владельцем adapter semantics.

## Инварианты

- Row-local codec/runtime incompatibility не влияет на независимые candidates.
- Source lineage mismatch, extraction generation mismatch и duplicate exact identity остаются фатальными `PlanningCandidateSnapshot::new` invariants.
- Alignment строит canonical plannable projection повторно и сравнивает полный exact+semantic набор и projection values только для планируемых rows.
- Rejection diagnostics не содержат URL/request material: correspondence выполняется через exact candidate identity.
- Snapshot, normalization inventory и request material не мутируются planning projection-ом.

## Regression и verification

- Functional regression: `candidate::planning_tests::unplannable_bare_hevc_row_keeps_independent_h264_candidate_available`.
- Он доказывает production normalizer acceptance обеих rows, сохранение H.264 в downstream planning snapshot, exact bare-HEVC rejection `RuntimeRequirement`, успешный alignment и совместимое поведение `planning_snapshot()`.
- PASS:
  - `cargo test -p service-ytdlp --locked` — 150 unit tests плюс все profile integrations;
  - `cargo test -p app-egui --locked` — 950/950;
  - `cargo clippy -p service-ytdlp -p app-egui --all-targets --all-features --locked -- -D warnings`;
  - `cargo +1.96.0 check --workspace --locked`;
  - `env RUSTDOCFLAGS=-Dwarnings cargo doc -p service-ytdlp --no-deps --locked`;
  - `cargo fmt --all --check`, `git diff --check`, Serena diagnostics.

## Audit record

- Исходный `user/project_health_audit_2026-08-22.md` обновлён: AUD-008 отмечен независимо подтверждённым, исправленным и закрытым 2026-08-23.
- Related: `mem:media-services/ytdlp-candidate-normalization-s19-2026-07-21`, `mem:media-services/web-playback-planner-s21c-2026-07-21`, `mem:core`.
