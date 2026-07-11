# render-core module layout (Session 22, 2026-07-11)

- `render-core/src/lib.rs` is a small compatibility facade. Its child modules are private and the existing public API remains available through explicit root `pub use` declarations, so consumers continue to use `render_core::Type`.
- Invariant owners:
  - `viewport.rs`: `RenderViewport` geometry and exclusion policy.
  - `color.rs`: color/HDR vocabulary, settings, and `ActiveColorPath` classification.
  - `shader_parameters.rs`: typed shader parameter schema and values.
  - `live_settings.rs`: preview/commit/rollback live-settings transaction and distinct typed failures.
  - `frame.rs`: neutral `RenderableFrame` and UI composition vocabulary.
  - `capabilities.rs`: renderer capability declarations and frame/video-output acceptance policy.
  - `diagnostics.rs`: neutral diagnostic DTOs and typed contract/output rejections.
- Cross-module references use the owning type through `crate::...`; there is intentionally no shared `types.rs` bucket.
- Focused unit tests live under `render-core/src/tests/<owner>.rs` and are attached from the corresponding owner module. Capability policy helpers stay private or `pub(crate)` only where their colocated tests need access.
- Session 22 was mechanical: public type names, root public paths, serde derives/field schema, capability policy, and dependency boundaries were not changed.
- Validation: `cargo test -p render-core --locked`; reverse dependents `capability-core`, `render-wgpu-video`, `render-wgpu-shell`, and `app-egui`; `scripts/check-refactor-guardrails.py`; `scripts/pre-pr-checks.sh`.
