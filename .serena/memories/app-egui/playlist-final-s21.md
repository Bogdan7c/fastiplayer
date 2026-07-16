# Playlist Session 21 final hardening (2026-07-16)

- Playlist v1 получил feature-scope PASS без product-code/API/boundary изменений. Полная D01–D81 матрица и evidence всех 39 prerequisite sessions: `user/playlist_queue_session_21_traceability.md`; итоговый статус и handoff: `user/playlist_queue_implementation_plan.md`.
- Narrow baseline: 1474/1474 выполненных hermetic tests PASS; 22 explicit media/network tests ignored и не считаются acceptance. Full PASS gates: fmt, Rust 1.96 locked workspace check, strict Clippy, strict rustdoc, all-feature tests, app no-default, MSRV 1.92, refactor guardrails, 30 policy tests и diff check.
- Session 21 исправила только два playlist-scope CI inventory gap: пять новых pure crates добавлены в blocking coverage policy; cargo-machete теперь получает exact все 37 workspace members и защищён `scripts/tests/test_dependency_audit_inventory.py`.
- D28 foundation остаётся NOT READY: `RUSTSEC-2026-0194/0195` у `quick-xml 0.39.3` через `wayland-scanner 0.31.10`; coverage relocation/ratchet после успешной suite/report generation. Baseline/exceptions/advisory ignores не менялись.
- Manual release smoke NOT RUN, потому что пользователь не передал explicit local video/audio paths и YouTube/direct URL. Не использовать owner-local assets автоматически; exact commands/checklist записаны в traceability.
- `player-core` остаётся queue-free; service/render ownership не изменён. Session 21 не добавляла O(N) per-frame/full-view clone или O(N²) bulk paths. Existing >800-line modules не трогались, потому что split без feature boundary был бы косметическим refactor.
- Связанные memories: `mem:core`, `mem:testing/coverage`, `mem:ci/github-actions`, `mem:app-egui/playlist-controller-s20`, `mem:app-egui/playlist-ui-s20`.
