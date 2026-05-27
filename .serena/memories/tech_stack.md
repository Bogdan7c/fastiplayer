# Tech Stack

- Language: Rust, workspace edition `2024`, workspace `rust-version = "1.85"`, Cargo resolver `2`.
- Main UI/window stack: `winit 0.30`, `egui 0.34`, `egui-winit 0.34`, `egui-wgpu 0.34`.
- Render/GPU: `wgpu 29` with `vulkan` feature; production renderer crates are `render-wgpu-video` (pure WGPU NV12/P010 video renderer/materializer boundary) and `render-wgpu-shell` (WGPU device/surface/egui composition shell).
- Media/container/audio dependencies: upstream `symphonia 0.6` with `all-formats`, `all-codecs`, `all-meta`; `cpal 0.15`; `opus 0.3`; `bytes`; `ringbuf`; `crossbeam-channel`; neutral audio contracts live in local `audio-core`.
- Error/log/config stack: `anyhow`, `thiserror`, `tracing`, `tracing-subscriber`, `serde` derive, `toml 0.9`, `directories 6.0`.
- Local patches via root `[replace]`: `cros-libva:0.0.12` -> `crates/cros-libva-patch`; `cros-codecs:0.0.6` -> `crates/cros-codecs-patch`. These are compatibility patches, not app-owned architecture.
- Local crates are wired through workspace dependencies; crate package `crates/config` publishes as `rustiplayer-config` / lib `rustiplayer_config`.
- Linux config path is `~/.config/rustiplayer/config.toml`; config schema version is `2`; unknown TOML fields are rejected and validation after serde is mandatory.
- SonarQube is local-only for project key `rustiplayer` at `http://127.0.0.1:9000`; scanner imports Clippy JSON from `target/sonar/clippy-report.json`.