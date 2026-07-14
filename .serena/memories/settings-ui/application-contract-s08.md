# Settings Apply Contract — Session 08

- Session 08 (2026-07-11) added the checked application matrix in `crates/rustiplayer-settings/src/application_contract.rs`.
- `SettingApplicationContract` maps every editable `AppConfig` descriptor to exactly one `AppRuntimeRoute`, `SettingStateOwner`, intent-based `SettingApplyMechanism`, rollback owner, and focused test scenario set.
- The mapping deliberately matches stable setting ids explicitly rather than by prefix. `every_editable_setting_has_one_checked_live_application_contract` iterates the generated registry, so a newly editable descriptor without a matrix row fails the focused test.
- `runtime_route_from_descriptor` validates that descriptor metadata and the checked matrix select the same project route before building an executor plan.
- Intent mechanisms are distinct: in-place state/policy update, worker reconfigure, renderer live update, audio output recreation, media source rebuild, video pipeline rebuild, renderer recreation, and preview promotion. The matrix contains no restart/deferred mechanism.
- `SettingsApplyOutcome<ApplyError, RollbackError>` keeps `Applied`, `Noop`, retryable `RuntimeBusy`, generation `Conflict`, apply failure, and combined apply+rollback failure typed and distinct. Busy/conflict documentation requires detection before mutation and forbids hidden queuing.
- Current executor behavior was intentionally not rewritten in Session 08: existing `DeferredTechnicalDebt`, `deferred_boundary_settings`, persistence order, and unsupported-player plumbing remain until their scoped implementation/removal sessions.
- Session 08B input: implement Player/MediaService/FrameServer rows through owner boundaries, including event-policy updates, worker reconfigure, audio/source recreation, and video pipeline rebuild with rollback and retryable busy/conflict.
- Session 08C input: implement RenderCommitted rows via controlled renderer recreation; preserve live-render preview rows and cover active leases/device-lost/restore failure.
## Session 08D — end-to-end settings transaction (2026-07-11)

- `SettingsController::apply` теперь выполняет validate -> generation/runtime preflight -> ordered runtime commit -> atomic TOML persistence -> final committed document/generation update.
- Runtime owners применяются в стабильном `AppRuntimeRoute` order; при failure completed prefix компенсируется в обратном порядке. Apply report хранит исходные route reports и отдельные typed rollback reports; rollback failure имеет `ApplyFinalState::RollbackFailed` и не скрывает исходную ошибку.
- Generation conflict и app/player/renderer busy preflight происходят до первой owner mutation, ничего не persist-ят и не создают hidden retry queue. Draft остаётся неизменным для явного повторного Apply.
- Persistence failure после полного runtime commit вызывает compensating runtime rollback; app-owned committed config snapshot синхронизируется только после успешной atomic TOML записи.
- `DeferredTechnicalDebt`, `deferred_boundary_settings`, `PersistedRuntimeDiverged`, generic player `unsupported_settings` и unreachable `RequiresControlledRebuild` удалены без compatibility aliases.
- `crates/app-egui/src/settings_runtime/transaction.rs` владеет end-to-end orchestration; route owners остаются в `route_apply.rs`, renderer lifecycle — в `renderer_recreation.rs`, visual UI получает только progress/success/failure status.
- Apply/OK сначала публикует busy/progress model на один UI frame, затем выполняет transaction. Retryable busy/conflict сохраняет draft и показывает явную подсказку повторить Apply; автоматического retry/hidden queue нет.
- End-to-end tests покрывают multi-group success, second-group failure, reverse rollback, rollback failure, preflight busy без persistence/queue, same-draft retry, preview promotion, persistence failure с compensation и repeated no-op.


## Session 08B — live apply player/media/decoder (2026-07-11)

- Player committed route теперь различает event policy, media-pipeline rebuild, audio-output recreate, decoder config и backend policy; прежний `deferred_boundary_settings` оставлен только как transitional S08D cleanup surface.
- App-owned committed snapshot синхронизируется только после полного успеха route reports; при owner failure snapshot сохраняет последнюю рабочую policy.
- Retryable seek/scrub/pipeline busy проходит через `AppRouteApplyResult::RuntimeBusy` и player group report без hidden apply queue.
- Event-scoped `start_paused` и seek-hotkey policy принимаются немедленно; codec/demux/network changes при active remote/local media используют controlled source reopen, а YouTube codec order выбирает первый поддержанный configured codec.
- Renderer recreation не входит в S08B; cross-route persistence ordering и удаление transitional debt остаются задачей S08D.

## Session 08C — controlled live renderer recreation (2026-07-11)

- `RenderCommitted` больше не возвращает `DeferredTechnicalDebt`: shell-owned coordinator транзакционно recreates renderer/materializer, сериализуется с surface events и возвращает typed busy/apply/rollback failures.
- Render committed snapshot меняется только после owner success; failure сохраняет old snapshot и допускает retry того же draft.
- Typed lifecycle и release invariants находятся в `mem:render-video/controlled-renderer-recreation-s08c`.
- Persistence-before-runtime и generic deferred cleanup остаются scoped задачей Session 08D.


## Session 10D — strong media reconfigure completion (2026-07-14)
- Active media reconfigure больше не считает command enqueue/Ready/authorization acceptance успехом: route ждёт correlated `Installed`, затем exact request/`MediaInstanceId` position+track restore и D52 intent outcome.
- `state::strong_media_open` коммитит app video pointers сразу после exact Installed, до fallible restore. Active source публикуется только после Installed.
- Любая ошибка после возможного install barrier возвращает `AppRouteApplyResult::PartialFailure`, включая post-Installed D52 dispatch/outcome, pointer invariant и restore failures. Поэтому `AppConfigSettingsDelegate::applied_route_count` включает failing route в reverse compensation; rollback повторно проходит тот же strong reinstall и ждёт terminal Installed. Доказанный pre-barrier failure возвращает `Failed` и не запускает лишний rollback.
- Focused classification tests закрепляют PartialFailure vs Failed; существующая transaction matrix сохраняет separate apply/rollback reports и persistence-failure compensation.