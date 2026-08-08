# S25 — controlled same-item candidate switch (2026-07-22)

## Ownership and entrypoint
- URL sidebar publishes only a secret-safe `UrlSidebarAction::SelectCandidate { generation, candidate_index }`. Exact `YtDlpCandidateSelection` tokens remain private inside `WebMediaStreamConfiguration`; generation+index validation is the boundary.
- `AppState` owns the single-flight `PendingSameItemCandidateSwitch` and constructs a fresh `YtDlpCandidateOpenIntent::Exact`. The open path re-extracts in background and uses the existing semantic `rematch_exact`; old playback continues until authorization wins.
- Playback-window/group-part/CUE semantics are retained through `ActiveMediaSource::wrap_reopen_request`. Detached active media is valid: no queue current is invented and runtime item preference carries `item_id: None`.

## Commit protocol and lineage
- The shared stepwise strong-open envelope has an explicit `PendingStrongLineageCommit::{NewLineageOrQueue, SameLineage}` policy and `PendingStrongMediaAdmission::{Playlist, SameLineage}`.
- Same-lineage staging never creates/accepts a queue reservation. Immediately after exact `Installed`, before post-install restore can fail, it calls `PlaylistRuntime::complete_same_item_candidate_switch`, whose only controller mutation is `PlaylistController::rebind_active_media_same_lineage`.
- S25 must never call `register_external_strong_install`: that API creates a new app lineage. Queue current, structural/traversal revisions, shuffle history/upcoming/cursor, Item ID and lineage remain unchanged.
- Pre-barrier stale/failure performs lossless cancellation, clears the matching selector and preserves old playback. After `EnqueuedAtPlayerOwner`, the existing CommitMustFinish protocol applies. Suspend sends `LifecycleSuspended` losslessly and drains either cancel-win or install-win; shutdown relies on existing process-owner cancellation/drop authority.

## Fresh restore
- At `ReadyToCommit`, immediately before authorization, the app validates exact active instance/binding and captures fresh position, Playing/Paused intent, numeric volume and selected tracks.
- After exact `Installed`, video/audio IDs are restored only if the new A/V inventory contains the same ID+kind. Subtitle selection uses its separate owner contract: `None -> Disabled`, `Some(id) -> Select(id)`; subtitle tracks are not part of `TrackSummarySnapshot`.
- `InstalledMediaStateRestore` now contains explicit `InstalledVolumeRestore::{KeepCurrent, Set(f32)}`; invalid volume is reported as typed `InstalledMediaRestoreFailureStage::Volume`.
- Position uses the existing player playback-window mapping, so a window-relative target is converted at the player boundary. Fresh playback intent is acknowledged after state restore.

## Render lifecycle
- Exact same-item `Installed` starts the existing backend-swap present-frame freeze before candidate pointer commit. Old renderer resources remain Arc-owned until the new render generation yields a frame.
- If the new candidate is audio-only, generation switch plus absence of a video track terminates the freeze and releases the old frame; same/cross backend paths keep the existing exactly-once candidate release rules.
- See also `mem:render-video/live-backend-swap-present-frame-freeze` and `mem:render-video/controlled-renderer-recreation-s08c`.

## Verification
- Focused tests cover fresh Playing/Paused capture, exact/disabled subtitle restore, applicable A/V tracks, exact volume and typed invalid-volume failure, playback-window relative seek, detached active source, compound part current, selector busy/stale/failure state, queue/traversal/shuffle/lineage preservation, and existing same/cross-backend/cancel/release scenarios.
- Final verification: 568 `player-core` tests PASS; 825 `app-egui --no-default-features` tests PASS; strict all-targets Clippy for both touched crates, default-feature app check, rustfmt, refactor guardrails, diff check and Serena diagnostics PASS.
## S36C3 generalized same-item component switch (2026-07-24)

- S25's transaction metadata is generalized to one `PendingSameItemSwitch` with typed `SameItemSwitchKind::{Candidate, Component}` over the same strong media-open envelope; candidate and component actions cannot race or occupy separate lifecycle slots.
- Component actions are validated by the app-owned model before start and become semantic-only intents over the exact active parent. Fresh preparation must install/rematch a fresh component catalog before the existing player barrier; there is no fallback to provider default when the requested semantic variant disappeared.
- Candidate reopen intentionally uses provider-default component selection and remains the only path that updates the item preferred-height override. Component reopen preserves the independent other A/V axis and never mutates global or per-item height preference.
- URL sidebar pending/error state is shared across candidate/component selectors. Exact request id, source lineage and fresh Installed component catalog are checked at completion; impossible post-Installed mismatches are logged as bounded invariant diagnostics and mapped to the secret-safe stale UI category.
- `PlaylistRuntime::complete_same_item_media_switch` retains the original same-lineage contract: no queue/traversal/shuffle mutation, no invented current item, and the same render freeze/commit-must-finish semantics.

## S35S live extension (2026-07-24)

- Same-lineage live restore now sends the captured old absolute position as `InstalledPositionRestore::RestoreLiveSameItemPosition`; app never reads/clamps the fresh DVR range.
- Player decides against the latest installed new-generation timeline: retained DVR targets use the existing exact seek lifecycle, while expired/no-DVR targets return typed `AdjustedToLiveEdge` and keep a `Live` checkpoint.
- Exact Installed → same-lineage rebind → restore/intent ordering, CommitMustFinish, selector cancellation and playlist/traversal/shuffle/lineage invariants are unchanged.
- Full contract: `mem:app-egui/live-same-item-candidate-switch-s35s-2026-07-24`.

## Production action/lifecycle bridge (2026-08-08)

- `SameItemSwitchAppPath` в `state/same_item_candidate_switch/lifecycle_bridge.rs` является единственным production orchestration path между resolved URL sidebar action и существующими strong lifecycle methods. `AppState::apply_url_sidebar_action`/start и `poll_same_item_switch` делегируют ему; это не параллельный reducer и не test-copy.
- Для borrow-safety bridge временно извлекает только `PendingSameItemSwitch`. URL controller остаётся внутри `AppState`, потому что strong `Installed` вызывает `record_installed_media_source` и обязан обновить этот controller до terminal selector effects. Panic rollback не является обещанием boundary; штатные terminal paths возвращают pending/controller в согласованное состояние.
- Functional tests используют настоящий `MediaOpenSourceRequest::YtDlp` и тот же production start/poll path через injected lifecycle port: Playing и Paused сохраняют position/intent и коммитят preference только после `Installed`; pre-barrier failure сохраняет playback/preferences и восстанавливает selector. Resolution/catalog component tests отдельно покрывают преобразование UI action в resolved request.
- Проверено: bridge 3/3, полный `app-egui` 934/934, strict all-targets Clippy, workspace `hermetic-ci` PASS, release build, rustfmt, refactor guardrails и diff check.

- Related memories: `mem:playlist/core`, `mem:app-egui/media-open-coordinator-s10c`, `mem:render-video/live-backend-swap-present-frame-freeze`, `mem:app-egui/live-same-item-candidate-switch-s35s-2026-07-24`.
