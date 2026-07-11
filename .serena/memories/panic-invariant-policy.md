# Panic/invariant policy

- Канонический краткий документ: `docs/panic-invariant-policy.md`.
- Production fallible boundaries (OS, thread, lock, file/network, decoded input, user config) возвращают typed error через boundary владельца; panic там запрещён.
- `expect` допустим только для compile-time/non-zero constants и доказанных private invariants с локальным объяснением.
- `is_some()` + `take().expect(...)` следует заменять structural `match`/`let Some`, если это делает ownership/invariant явным.
- Poisoned lock нельзя восстанавливать через `PoisonError::into_inner`, когда poison означает потерю доверия к защищённым инвариантам.
- Session 11 baseline (2026-07-11): production-only `cargo clippy --workspace --lib -- -W clippy::unwrap_used -W clippy::expect_used` до правок дал 53 finding-а (2 unwrap, 51 expect); после bounded fixes — 38 expect и 0 unwrap. Оставшиеся 38 сгруппированы по crate в policy и должны исправляться отдельными crate-local work packages, не механическим workspace churn.
- VA-API resource-pool poison: backend-local `VaapiResourcePoolPoisonError` классифицируется как fatal decoder error; `VaapiVideoDecoder` использует единый fail-closed lock helper, reconfigure/format-change протягивают error и останавливают lifecycle. `VideoDecodeThread::resource_pool_stats` при poison записывает sticky `DecodeThreadError`. Controlled recovery запрещён; repeated cleanup lock attempts возвращают тот же typed class без panic.
- Focused tests: `decoder::tests::poisoned_resource_pool_returns_typed_fatal_error_on_every_lock_attempt` и `decoder_thread::tests::poisoned_resource_pool_stats_marks_decoder_thread_fatal`.