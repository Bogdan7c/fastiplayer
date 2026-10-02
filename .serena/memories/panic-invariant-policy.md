# Panic/invariant policy

- Канонический документ: `docs/panic-invariant-policy.md` (раздел «Машинная проверка»).
- Проверка: `[workspace.lints.clippy]` unwrap_used/expect_used/panic = warn в корневом Cargo.toml, каждый member `[lints] workspace = true`, CI `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`. Тесты освобождены `clippy.toml` (allow-*-in-tests); integration test crate-ы — `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = ...)]` в корне; модули с составным cfg объявлять `#[cfg(test)] #[cfg(unix)]`, иначе clippy не считает их тестами.
- Порядок действий для нового места: 1) убрать причину структурно (`take_if`, `next_if`, `get_or_insert_with`, `Option::insert`, `first_chunk` массивы, enum вместо согласованных Option, `non_zero()` у NonZero-обёрток, `const { NonZero::new(C).expect(..) }`); 2) иначе `#[expect(clippy::expect_used, reason = "доказательство")]` на `let`-операторе или функции (не на выражении-присваивании: E0658).
- `rustfmt` переносит атрибуты длиннее ~70 символов аргументов; в файлах у предела размера писать короткий reason в одну строку.
- Poison: снимать (`unwrap_or_else(PoisonError::into_inner)`) только если mutex не охраняет данные или каждая критическая секция атомарна по инварианту (задокументировать helper-ом); иначе typed fatal error (VA-API resource pool — fail-closed).
- Фокус-тесты: `worker::tests::snapshot_read::poisoned_publication_barrier_still_delivers_latest_snapshot_to_handle`, `app_wake::tests::poisoned_mailbox_still_delivers_completion_to_ui_drain`, media-prefetch `append_beyond_u64_offsets_is_rejected_without_changing_buffer`, HDS `every_truncated_prefix_of_valid_bootstrap_is_rejected_without_panic`.
- Связано: `mem:code-health/project-health-cleanup-2026-10-02`.
