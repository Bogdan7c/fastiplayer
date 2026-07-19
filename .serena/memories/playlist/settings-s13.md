# Playlist settings — Session 13

Session 13 completed PASS on 2026-07-15. This memory complements `mem:core`, `mem:playlist/discovery`, `mem:playlist/state`, `mem:settings-ui/application-contract-s08`, and `mem:config/schema-store-decomposition-s23`.

## Ownership
- `rustiplayer-config::PlaylistConfig` owns strict/defaulted schema, defaults, enum ids, metadata and timing validation.
- `rustiplayer-settings` owns dedicated `AppRuntimeRoute::Playlist`, typed full-policy payload and explicit application matrix rows.
- `PlaylistRuntime` owns one process-lifetime `PlaylistSettingsOwner`: committed future discovery policy/revision, error policy, new-queue playback default, Previous threshold, queue debounce port, resume-checkpoint interval port and at most one staged settings transaction. Resume interval live apply/rollback preserves the latest pending position snapshot; full contract: `mem:playlist/resume-position-sidecar-2026-07-19`.
- `PlaylistController` still owns existing queue/repeat/error runtime. Live playback-default changes never mutate an existing queue; startup policy initializes only a new queue before future Session 14 persisted-state restore.

## Transaction and D62
- Approved transaction order is validate -> preflight -> reversible apply -> atomic persistence -> synchronous/idempotent/infallible finalize -> settings controller commit -> app committed snapshot sync.
- `load_siblings=false` stage freezes an exact typed discovery scope without cancellation. Rollback resumes the exact scope; finalize cooperative-cancels it only after persistence. Enabling does not create a scan; filter/load changes advance a typed checked future-policy revision, and exhaustion rejects before any runtime mutation.
- A debounce reconfigure failure followed by failed exact resume is `PartialFailure`, so compensation remains mandatory. Rollback retains staged state until debounce restore and exact resume both succeed. Repeated finalize is a no-op.
- Session 14 supplies the real state-worker/debounce adapter. The Session 13 detached port is stateful policy storage and represents absent worker/scan explicitly; no disk store or production discovery coordinator was wired.

## Verification
- PASS: 79 config, 16 rustiplayer-settings, 25 settings-core, 393 app no-default, 52 discovery and 33 state tests; strict focused Clippy; fmt; locked workspace check; refactor guardrails; diff check and clean Serena production diagnostics.


## Session 14 persistence/shutdown integration (2026-07-15)

- Live debounce reconfigure remains transactional with apply/reschedule and rollback. PlaylistRuntime owns the committed debounce schedule used by its state worker; process shutdown joins active and retired dynamic-option jobs under the shared absolute deadline before releasing the app lease.
- Full owner order and limitations: `mem:app-egui/playlist-persistence-s14`.
