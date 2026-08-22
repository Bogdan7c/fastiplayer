# AUD-001 post-Installed strong-open compensation (2026-08-23)

## Причина и выбранная семантика

- Независимая read-only проверка подтвердила AUD-001 из `user/project_health_audit_2026-08-22.md`: после exact player `Installed` generic strong-open мог завершиться обычным `Err` на restore/playback-intent и удалить `PendingStrongMediaOpen`, не освободив media и не сняв controller reservation.
- Выбрана compensation semantics: strong-open остаётся успешным только после restore + playback-intent + lineage/domain commit. Ранний controller commit сразу на `Installed` не применяется.
- После barrier-а ошибка больше не является terminal сама по себе: сначала обязан завершиться exact release/reconciliation. Cleanup failure не маскируется исходной ошибкой и является fatal invariant.

## Ownership и boundaries

- `PendingStrongMediaOpenPhase::PostInstalledRelease` владеет `InstalledSingleMediaOpen`, исходным `StrongMediaOpenError` и `InstalledMediaReleaseReceipt` между poll-вызовами.
- Player-core остаётся единственным owner exact release по `MediaInstallRequestId + MediaInstanceId`. Только matching `InstalledMediaReleaseOutcome::Applied` разрешает reconciliation; `Absent`, `StaleInstance`, `Failed`, потеря receipt и dispatch failure дают `PostInstalledCompensationFailed`.
- `PlaylistController::reconcile_released_post_installed_candidate` находится в `playlist_runtime/controller/install/post_installed_compensation.rs`. Boundary abort-ит opaque install token у owner-а, сохраняет manual failure или retained automatic traversal plan, снимает `AuthorizationInFlight`, очищает ложную active identity и публикует `Stopped`.
- `PlaylistRuntime::reconcile_released_post_installed_candidate` публикует dirty mutation и очищает suspended active source/checkpoint.
- `AppState::clear_released_installed_media_source` очищает app source, cached present frame и stale player snapshot/timeline только для matching released instance.
- Same-lineage `complete_same_item_media_switch` перенесён после успешного playback-intent acknowledgement. До этого controller сохраняет old identity; при failure release B не требует rollback частичного rebind-а.
- Blocking legacy strong-open callers используют ту же semantics через `state/strong_media_open/compensation.rs`.
- `StrongMediaOpenError::PostInstalledCompensated` разрешает playlist navigation recovery; `PostInstalledCompensationFailed` никогда его не разрешает. Settings по-прежнему видит, что install barrier пересекался.

## Проверки

- `playlist_runtime::controller::tests::released_post_installed_candidate_unblocks_next_exact_install`: A Installed -> B release reconciliation -> structural lock снят -> C exact Installed.
- `playlist_runtime::controller::tests::failed_post_installed_release_is_fatal_instead_of_unlocking_reservation`: неизвестный release outcome не даёт false recovery.
- Existing player functional release: `worker::staged_media_install::tests::exact_restore_rejects_precommit_and_stale_instance_then_applies_after_installed`.
- PASS: `cargo +1.96.0 test -p app-egui --no-default-features --locked` (950), strict app no-default all-target Clippy, `cargo +1.96.0 check --workspace --locked`, rustfmt, refactor guardrails, module-size guardrail и diff check.

## Ограничение

- Full real URL/hardware/GPU materialized/render-submitted matrix не заявлена этим fix-ом и остаётся AUD-013.
- Serena rust-analyzer может кратко показывать stale signature/removed-field diagnostics в новых linked files; Cargo check/test/Clippy являются authoritative и прошли.
