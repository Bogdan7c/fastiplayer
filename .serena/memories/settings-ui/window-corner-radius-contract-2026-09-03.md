# Settings contract: window corner radius (2026-09-03)

- Setting id/path: `ui.window.corner_radius_px`.
- Config owner: `UiWindowConfig.corner_radius_px: u16`.
- Default: 12 logical px. Validation: 0..=24 inclusive. Metadata: integer editor, step 1, unit px, group UI -> Window, apply route `ui.apply`.
- Meaning: 0 disables rounding; max/fullscreen always resolve to square; compositor alpha fallback is square. Russian label/description/help are declared at the field and default TOML contains the field/comment.
- Schema version stays v10. Old schema-v10 TOML without the field deserializes to 12 through `serde(default)` and is not rewritten merely by startup loading.
- Runtime owner reads only `CommittedConfigSnapshot::window_corner_radius_points()`. Draft mutation is intentionally invisible to rendering; Cancel preserves the previous value; successful Apply and OK update/sync the committed snapshot.
- Application contract routes this setting to UiShell with StateUpdateInPlace. It does not require renderer recreation because the transparent-capable surface/pipeline is created at startup, while per-frame radius is a lightweight typed input.
- Relevant focused tests:
  - config defaults, 0/12/24 validation, 25 error path, old v10 omission, generated/default documents, metadata.
  - app settings transaction test `window_corner_radius_activates_only_after_successful_apply` covers draft, Cancel, Apply and OK.
  - app policy tests in `window_corner_policy.rs`.
- Do not add live preview without an explicit product/architecture decision: current contract is Apply/OK only.