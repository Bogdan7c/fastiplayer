# S42 app-egui production boundary splits — executor wave 3 (2026-08-27)

## Поведение и API

- Это behavior-neutral relocation: публичный и `pub(crate)` API, имена inherent methods, typed outcomes, runtime ordering и error semantics не изменены.
- `crates/app-egui/src/frame_prepare/settings_runtime_adapter.rs` остаётся владельцем runtime I/O/mutation и active-media reopen. Чистая классификация setting IDs и typed error/report projection перенесены в canonical private child `crates/app-egui/src/frame_prepare/settings_runtime_adapter/reconfigure_projection.rs`; существующий outer `#[path]` требует минимальный explicit child path в adapter owner-е.
- `crates/app-egui/src/web_media_stream_model/sidebar_controller.rs` теперь владеет `UrlSidebarController`, `SafeErrorState`, `ItemOverrideState` и exact-lineage/single-flight ephemeral transitions. `web_media_stream_model.rs` по-прежнему строит Installed-source-authoritative read-only model; crate path `web_media_stream_model::UrlSidebarController` сохранён private re-export-ом.
- `crates/app-egui/src/state/strong_media_open/pending/admission.rs` владеет всеми `begin_*` ingress methods и Prepared→player/controller admission/staging. `pending.rs` сохраняет `PendingStrongMediaOpenPhase`, poll authority, Install barrier, post-Installed compensation и terminal ownership.

## Инварианты

- Settings failure после потенциального Install barrier остаётся `AppRouteApplyResult::PartialFailure`; только доказанный pre-barrier failure остаётся `Failed`.
- URL sidebar pending/error/override остаются fenced по exact generation, source lineage и playlist item; Installed source очищает ephemeral state, component switch не меняет item preference.
- Strong-open admission сохраняет exact candidate/resource ownership, queue/same-lineage distinction, structural-invalidation cancellation, deferred rejection и compensation paths.

## Размеры и проверки

- Итоговые production line counts: settings adapter/projection `687/202`; web stream model/sidebar controller `731/126`; pending/admission `469/492`. Все owner-ы ниже hard limit 800 без baseline changes.
- Focused PASS: settings projection 3/3; web stream model 15/15; strong-media-open 16/16.
- Full PASS: app-egui no-default 1000/1000; all-features 1000/1000; strict no-default/all-features all-target Clippy; app no-default check; rustfmt; diff check; refactor guardrails.
- Global `scripts/check_s42_guardrails.py` остаётся red из-за repository-wide stale/oversized baseline entries, но ни один из шести app-wave production paths не входит в failure list. Baseline намеренно не менялся.
- Historical `runtime-coverage-s41.json` сохранён immutable. `cross_provider_integration_s41.rs` теперь по существующему coordinator-pattern exact-map-ит старый settings adapter evidence path в canonical nested projection owner; exact symbol assertion и validator strictness сохранены. S41 integration 3/3 и final acceptance S42 24/24 PASS.
- Serena diagnostics clean для adapter owner-а, canonical nested projection child-а, sidebar split-а, strong-open split-а и S41 validator-а после reactivation; stale `unlinked-file` hint устранён.