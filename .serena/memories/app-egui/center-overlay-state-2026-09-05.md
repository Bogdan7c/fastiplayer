# Central overlay: start hint is Idle-only

- `state/ui_runtime.rs::AppState::render_center_overlay` receives existing `PlaybackState` from the frame's immutable `PlayerSnapshot`, rather than `is_playing: bool`. Player state/lifecycle remain owned by player-core; no commands, config schema or public API changed.
- Start hint `Open a file or URL to start` is painted only in Idle. Paused, Playing, Opening, Buffering, Seeking, Scrubbing, Draining, Ended, Stopped and Failed do not paint it. Non-Playing must never be used as a proxy for no media.
- Priority remains queue replacement confirmation > import preview > error > pending > Idle hint. Renderer returns typed confirmation/import actions only after actual input.
- Functional tests in `state/center_overlay_tests.rs` run the real egui renderer for two frames and inspect painted text shapes for all states and overlay priority. They verify no unsolicited action, unchanged snapshot, preview, confirmation and queue revision. Run `cargo test -p app-egui --locked center_overlay`.
- Small local correction remains in the existing method to keep the single overlay-priority invariant together; test implementation is a separate child module. No neighboring refactor.
