# Settings infrastructure decomposition — Session 27D (2026-07-12)

- Session 27D performed a behavior-neutral census/decomposition after Session 08D.
- `settings-core` owner map remains: `metadata.rs` owns neutral descriptor/editor validation, `registry.rs` owns registry/accessors/nested composition, and `controller.rs` owns the coherent neutral draft/preview/generation/apply/rollback transaction state machine. `controller.rs` was deliberately not split solely by line count.
- `settings-derive/src/lib.rs` is now a thin proc-macro facade. Implementation boundaries are:
  - `codegen/parsing.rs`: syn attribute/metadata parsing with original spans;
  - `codegen/validation.rs`: semantic Rust field type/editor compatibility checks;
  - `codegen.rs`: derive orchestration and registry/descriptor/accessor token generation.
- `fastiplayer-settings/src/lib.rs` is now a stable public facade. `transaction.rs` owns AppConfig validate/atomic persist/runtime-applier delegation. `routing.rs` owns project-specific route taxonomy, diff grouping, typed owner payload construction, and focused routing tests. `application_contract.rs` remains the exhaustive Session 08 application matrix.
- Public exports, generated token behavior, trybuild diagnostics, strict schema coverage, typed live-apply mechanisms, busy/conflict distinctions, and reverse compensation semantics are unchanged.
- Census and follow-up prompts live in `user/settings_infrastructure_census_session_27d_2026-07-12.md`.
- Verification: settings-core tests, settings-derive generated schema + trybuild, fastiplayer-settings tests, fastiplayer-config strict registry/schema tests, app-egui settings_runtime tests, locked workspace check, strict clippy for settings crates, strict rustdoc, refactor guardrails.
