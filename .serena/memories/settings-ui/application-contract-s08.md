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
- Session 08D input: make the typed outcome transactional end-to-end, move persistence after successful runtime commit, compensate earlier owner commits in reverse order, then remove deferred debt vocabulary and UI string-based status plumbing.


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
