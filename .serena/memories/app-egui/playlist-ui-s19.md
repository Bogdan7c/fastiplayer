# Session 19 — Playlist toolbar/forms/progress UI (2026-07-16)

## Ownership и frame boundary
- `ui/playlist/actions.rs` defines typed `PlaylistAction`; egui only renders immutable models and accumulates actions. `playlist_action_runtime::apply_playlist_actions` runs after the egui closure in `frame_prepare::render_frame`.
- `PlaylistUiFrameModels` groups the confirmation, toolbar/forms/progress, and global transport snapshots from the same frame. UI never borrows the controller while applying actions.
- `PlaylistUiInteractionOwner` lives inside process-lifetime `PlaylistRuntime`. It owns the D48 URL draft, focus request, safe validation error, async multi-file picker lifecycle, and bounded safe feedback. Sidebar hide/show and renderer-bound `AppState` recreation do not destroy or submit the draft.
- Animation copies receive disabled egui plus temporary `PlaylistUiState`/discarded output. They cannot emit actions, consume the URL focus request, replace the viewport anchor, or publish demand hints.

## Toolbar and forms
- Toolbar actions: async multi-file Add, inline URL Add/Cancel with Enter/Escape, Clear, repeat, shuffle, stop-after-current, six-key ascending/descending Sort menu, and explicit D80 “Перейти к текущему”.
- rfd 0.15.4 `AsyncFileDialog::pick_files` runs in one owned thread and publishes one terminal mailbox completion. Cancel/empty selection does not mutate the queue. No dialog flag is added to periodic background redraw; the wake event drives result drain.
- Raw URL exists only in the process draft/model and the redacted `PlaylistUrlDraftText` action. Draft state and actions have redacted Debug; validation/status/accessibility use bounded safe text. Sensitive/composed D15+D79 confirmation remains the single central overlay and returns only exact typed Confirm/Cancel.
- D80 is a one-shot AppState UI intent. It scrolls/focuses only after the explicit action and supports either the stable Item-ID row or a focusable tombstone presentation. No automatic active-row scroll was added.
- Existing global D46 Undo remains the only Undo action; the Playlist section does not duplicate it.

## Progress/status/cancellation
- Interaction model exposes foreground manual Add, sibling discovery, or metadata Sort progress as stage + processed/total + typed cancel scope. Sort is disabled during preparation.
- Manual Add terminal UI shows “Добавлено X из Y” plus bounded aggregate failure counts; paths and locators are never rendered. Cancelled/invalidated Sort uses the exact D44 wording and reports metadata-updated count without claiming reorder.
- D50 shows direction-specific text only for a real manual wait. Shuffle Previous without factual history remains disabled by the global transport model and never fabricates a wait.
- D55 reuses one typed cancel boundary. Controller exposes only the D56 accessibility fact for an already-Ended origin, so the tooltip can say cancellation leaves playback stopped without the renderer deriving the outcome.
- D58 stop-after-current delegates to the controller; enabling reports cancellation of a pending transition, while disabling explicitly does not promise resurrection. D69 Retry delegates to persistence scheduling and does not implement UI timers/backoff or block playlist editing.

## Verification and next scope
- Session 19 verification: 561 `app-egui --no-default-features` tests; strict app Clippy all targets with `-D warnings`; `cargo fmt --all --check`; `cargo check --workspace`; refactor guardrails; `git diff --check`.
- Focused additions cover ordered typed action drain, hermetic multi-file Cancel/Selected exactly-once terminal handoff and duplicate-open rejection, consumed inline Enter, URL invalid/sensitive/stale-confirmation lifecycle and redaction, disabled animation-copy focus/action isolation, all six Sort keys, exact progress/wait text, and D56 tooltip.
- Исторический scope Session 19 не включал row interactions. Session 20 теперь завершена; актуальный UI contract: `mem:app-egui/playlist-ui-s20`. Следующая разрешённая playlist session — Session 21.
- Complements `mem:app-egui/playlist-ui-s18`, `mem:app-egui/playlist-transport-s18a`, `mem:app-egui/playlist-desktop-transport-s18b`, and the handoff in `user/playlist_queue_implementation_plan.md`.
