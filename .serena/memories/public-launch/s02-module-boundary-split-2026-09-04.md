# Public launch S02 — точечные module boundary splits (2026-09-04)

## Результат

- `crates/app-egui/src/state.rs` сокращён с 804 до 777 строк.
- `crates/web-media-dash/src/discovery.rs` сокращён с 834 до 444 строк.
- Refactor-only: public product/API surface, playback semantics, error variants, track selection и transport/accounting не менялись.

## App startup/wake ownership

- `crates/app-egui/src/state/startup_context.rs` теперь владеет `AppStateStartupContext`, приватным `PlayerTimelineWakeBridge` и intent-фабрикой `player_timeline_wake_bridge`.
- `AppState` остаётся единственным владельцем application state. Новый child-модуль не читает и не меняет поля `AppState`.
- `state.rs` сохраняет прежний internal path через `pub(crate) use startup_context::AppStateStartupContext`; более широкая visibility не добавлена.
- Поля startup context имеют только `pub(super)`, что сохраняет прежнюю эффективную видимость внутри `state`.
- Functional boundary test: `state/startup_context.rs::tests::startup_timeline_wake_bridge_reaches_application_player_boundary` вызывает настоящий `player_core::PlayerWorkerTimelineWake` trait object и настоящий `AppWakePort`, проверяя delivery точного `AppWakeOwner::PlayerTimeline`.
- `state/tests.rs::state_source_for_architecture_tests` включает новый owner-файл, поэтому source guards не теряют перенесённый код.

## DASH lane-proof ownership

- `crates/web-media-dash/src/discovery/lane_proof.rs` теперь владеет `ProviderLaneProof`, его `DashRepresentationLaneProofPort` impl, bounded parallel proof, `prove_tracks`, `exact_track`, validators, descriptor builders, codec matching и local component-probe error mapping.
- `discovery.rs`, `discovery/native_vod.rs` и `discovery/native_live.rs` остаются orchestration layer: fetch/parse/catalog/open-state остаются на прежнем уровне.
- Orchestration создаёт proof через именованный `ProviderLaneProofContext` и `ProviderLaneProof::new`; storage fields реализации приватны. Вся новая cross-child visibility ограничена `pub(super)`, что эквивалентно прежней доступности из descendants `discovery`.
- Focused owner tests в `discovery/lane_proof.rs` закрепляют positive audio proof, absent track -> `UnsupportedTrackShape`, invalid codec -> `ManifestEvidenceConflict`, cancelled probe -> `Cancelled`, ordinary transport failure -> `TransportUnavailable`, invalid component track shape -> `UnsupportedTrackShape`, generic demux/container failure -> `UnsupportedContainer`.
- Existing functional consumer coverage сохранена в `crates/app-egui/src/media_open/web/tests/native_dash_vertical.rs` и `native_dash_live_vertical.rs`; S02 прогнал все пять VOD/live N14A/N14B/error verticals до decoder/WGPU/audio consumer и exact accounting.

## Verification

PASS:
- `cargo test -p app-egui startup_timeline_wake_bridge_reaches_application_player_boundary --all-features --locked`;
- `cargo test -p app-egui state:: --all-features --locked` (89 tests);
- `cargo test -p player-core paused_idle_worker_wakes_on_sliding_live_window_and_publishes_latest_snapshot --all-features --locked`;
- `cargo test -p web-media-dash --all-targets --all-features --locked` (45 unit + 4 dynamic + 3 live runtime + 4 catalog);
- `cargo test -p app-egui media_open::web::tests::native_dash --all-features --locked` (5 functional verticals);
- `python3 scripts/check-refactor-guardrails.py`;
- strict Clippy для `app-egui` и `web-media-dash`, all targets/features, `-D warnings`;
- `cargo +1.96.0 check --workspace --all-targets --all-features --locked`;
- `cargo fmt --all -- --check`;
- `git diff --check`;
- Serena symbols/references/implementations/diagnostics self-review.

## Связанные memories

`mem:core`, `mem:app-egui/state-split`, `mem:app-egui/wake-runtime-s10a`, `mem:media-services/native-dash-vod-n09-2026-09-01`, `mem:media-services/native-dash-live-n10-2026-09-01`.
