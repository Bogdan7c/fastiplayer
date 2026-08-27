# Settings Apply Contract — Session 08

## S20Q — global preferred video height (2026-07-21)

- `yt_dlp.preferred_video_height` имеет explicit checked contract `MediaService / MediaSourceLifecycle / MediaSourceRebuild` с pipeline test scenarios.
- Apply вызывает existing strong active-YtDlp reopen с reselection; quality-only route не перестраивает local/direct source, mixed network+quality сохраняет remote rebuild. Persisted config содержит только global preference; per-item override остаётся runtime-only.
- Полный config/selection contract: `mem:config/schema-v7-quality-preference-2026-07-21`.


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

## Session 13 — post-persist finalize и Playlist route (2026-07-15)
- `SettingsController::apply` теперь выполняет validate -> preflight -> reversible runtime apply -> atomic persist -> infallible synchronous/idempotent `finalize_committed` -> committed document/generation update.
- Finalize не возвращает ошибку и не вводит divergence recovery. Persistence failure вызывает прежний reverse rollback и finalize не запускает.
- App committed config snapshot удалён из persister-а и синхронизируется `SettingsRuntime` только после runtime finalize и возврата `FullyApplied`, то есть после controller committed document/generation update.
- Добавлены отдельные `AppRuntimeRoute::Playlist` и `SettingStateOwner::PlaylistPolicy`; все шесть `playlist.*` ids имеют explicit checked contracts. Debounce использует `WorkerReconfigure`, остальные — `PolicyUpdateInPlace`; restart/deferred/hidden queue отсутствуют.
- App D62 owner contract описан в `mem:playlist/settings-s13`.


## Runtime-originated single-setting commit — remembered sidebar width (2026-07-17)

- `SettingsController::commit_runtime_setting(RuntimeSettingCommitRequest)` — intent boundary для атомарного изменения одного setting из runtime UI, а не из обычного draft Apply.
- Boundary строит requested document от committed snapshot и проходит существующий validate -> preflight -> ordered runtime apply -> atomic persistence -> finalize/rollback pipeline. Поэтому соседние незавершённые draft/preview значения никогда случайно не persist-ятся.
- После успеха только тот же setting синхронизируется в открытом draft и preview snapshots; остальные draft/preview состояния сохраняются. Same-route baseline rebases только если он был актуален до runtime commit; уже существующий настоящий generation conflict не маскируется.
- Failure полностью восстанавливает controller committed/draft/preview/generation baselines; app-owned live state дополнительно откатывается к committed snapshot владельцем host.
- `SettingsRuntime` использует boundary для `ui.sidebar.width_points`: latest-only debounce 500 ms, same rounded value не создаёт запись, deadline участвует в event-loop wake, manual width edit/Apply/OK сначала force-flush pending, Cancel соседних изменений не отменяет committed resize.
- Pending runtime resize force-flush-ится перед suspend и штатным shutdown. Focused tests покрывают coalescing, draft/preview preservation, genuine conflict preservation, no-op same-field sync, persistence rollback и drag -> Apply/Cancel ordering.


## AUD-019: canonical next-item preload settings route (2026-08-27)

- Все четыре editable ID `playlist.next_item_preload_{enabled,budget_mb,lead_time_ms,max_hold_ms}` имеют явный единый контракт: `AppRuntimeRoute::Playlist`, owner и rollback owner `SettingStateOwner::PlaylistPolicy`, механизм `SettingApplyMechanism::PolicyUpdateInPlace`, focused suite `POLICY_TESTS`.
- `route_diff` агрегирует четыре изменения в одну typed `PlaylistRuntimeSettingsUpdate` с полным `PlaylistConfig`; preload policy не дробится на независимые runtime mutations.
- Forward executor вызывает `apply_playlist_runtime_settings(update)`, а compensating path — только `rollback_playlist_runtime_settings()`. Typed `Applied/Noop/PartialFailure/Failed/Busy/Conflict` проходит через общий route report без сведения к `bool`.
- Транзакционный порядок подтверждён функционально: success = один apply -> atomic persistence -> finalize -> committed snapshot; persistence failure = один apply -> ровно один rollback, без finalize, committed config остаётся прежним.
- Проверки: focused contract/routing/settings tests, `app-egui` 1005/1005 для `--no-default-features` и `--all-features`, strict clippy обеих app matrices и `rustiplayer-settings`, S41/S42 acceptance, format/refactor guardrails. Полный `cargo test -p rustiplayer-settings --locked` не заявляется зелёным: 17 passed/1 failed на внешнем незакрытом контракте `yt_dlp.vod_endpoint_recovery_enabled`.

## AUD-009 — checked VOD endpoint recovery settings contract (2026-08-27)

- Пять exact editable IDs `yt_dlp.vod_endpoint_recovery_{enabled,max_consecutive_attempts,initial_backoff_ms,max_backoff_ms,stable_reset_ms}` явно перечислены в checked application matrix без prefix-match.
- Каждый ID имеет один контракт: `AppRuntimeRoute::MediaService`, owner и rollback owner `SettingStateOwner::MediaOpenPolicy`, `SettingApplyMechanism::PolicyUpdateInPlace`, focused suite `POLICY_TESTS`, включая `EffectOnNextNaturalEvent`.
- Live apply меняет policy только для следующего естественного expiry claim-а; уже захваченная recovery-цепочка продолжает использовать свой immutable policy snapshot. Новый owner enum, restart/deferred mechanism и немедленное вмешательство в claimed recovery не добавлялись.
- `runtime_route_plan_from_diff` агрегирует одновременное изменение всех пяти полей в ровно один `MediaService` route: единственный source route `yt_dlp`, exact registry-stable ordered набор ID, одна группа `MediaYtDlp` и полный целевой `YtDlpConfig` внутри существующего `MediaServiceRuntimeSettingsUpdate`.
- Exact contract и routing regressions вынесены в focused private children `application_contract/tests/vod_endpoint_recovery.rs` и `routing/tests/vod_endpoint_recovery.rs`; центральный `application_contract.rs` остаётся 680 строк, legacy-ratcheted `routing.rs` — ровно 1939.
- Проверки: `rustiplayer-settings` 20/20 и doc-tests 0/0; config registry 1/1; strict all-target Clippy; rustfmt/diff/refactor/S42 guardrails; S41 3/3; S42 24/24; Serena diagnostics чисты.