# Window corner presentation contract (2026-09-03)

## Ownership and data flow
- rustiplayer-config owns user intent in `ui.window.corner_radius_px: u16`, default 12 logical px, valid 0..=24. Schema remains v10 because the field is additive and `UiWindowConfig` uses `serde(default)`.
- Settings changes use the existing `ui.apply` route. Only the committed snapshot is visible to frame rendering; draft edits and Cancel must not change the active window contour.
- app-egui owns native window-state policy. `CommittedConfigSnapshot::window_corner_radius_points()` provides configured intent; `window_corner_policy::resolve_window_corner_mask` returns square for radius 0, maximized, or fullscreen, and restores the committed radius when returning to normal state.
- render-wgpu-shell owns surface alpha selection and final composition. `RenderFrameInput::window_corner_mask` is the typed frame boundary. Do not move this responsibility into render-wgpu-video; the video viewport, aspect ratio, letterbox and exclusion rectangles are independent of desktop-window shape.

## Window and surface lifecycle
- The winit window is created with `.with_transparent(true)` from the start, which is required for X11 transparency behavior.
- Rustiplayer passes `SurfaceAlphaPreference::TransparentPreferred` both during initial renderer construction and controlled renderer recreation.
- Alpha selection order for TransparentPreferred is PreMultiplied, PostMultiplied, Opaque, Inherit. Empty capabilities remain an initialization error.
- Only PreMultiplied/PostMultiplied produce a `SurfaceAlphaEncoding` and create `WindowCornerMaskRenderer`. Opaque/Inherit keep the app running, warn once, and use a square/no-pass fallback to avoid black corners.

## GPU composition invariants
- Frame order is video/clear -> egui overlay -> window corner mask -> queue submit -> present.
- `WindowCornerMaskRenderer` lives in `crates/render-wgpu-shell/src/window_corner_mask.rs`; its WGSL is `window_corner_mask.wgsl`.
- The pass loads the current surface attachment and analytically multiplies coverage for a rounded rectangle. Radius is logical points converted by pixels_per_point and clamped to half the smaller physical surface dimension. AA width is one physical pixel.
- Premultiplied targets multiply destination RGB and alpha by coverage. Postmultiplied targets preserve straight RGB and multiply alpha only.
- Radius zero, max/fullscreen policy, or unsupported transparent alpha skips the pass. No intermediate texture, margin, border, shadow, or viewport adjustment exists.
- Transparent visual corners remain part of the rectangular native input/resize region.

## Diagnostics and tests
- `RenderFrameStageTimings::window_corner_mask` is a distinct stage and participates in slowest-stage reporting.
- The functional headless GPU test in window_corner_mask.rs executes the real render pass, reads back RGBA, verifies transparent corners, unchanged center/safe button areas, intermediate AA alpha, both alpha encodings, radius 24 safety, and square no-op.
- Surface selection tests cover Pre > Post, Post-only, Opaque and Inherit fallback, empty capabilities, and opaque policy.
- App policy tests cover normal/zero/max/fullscreen/restore.