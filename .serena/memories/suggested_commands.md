# Suggested Commands

- Build/check whole workspace: `cargo check --workspace`.
- Run app shell: `cargo run -p app-egui` (single binary target `rustiplayer`; explicit form: `cargo run -p app-egui --bin rustiplayer`).
- Run app with media path or URL: `cargo run -p app-egui -- /path/to/media.webm` or `cargo run -p app-egui -- 'https://www.youtube.com/watch?v=VIDEO_ID'`.
- Focused tests: `cargo test -p player-core`; capability/codec policy tests: `cargo test -p capability-core -p codec-core`; render split checks: `cargo test -p render-wgpu-video`, `cargo test -p render-wgpu-shell`, then `cargo check -p app-egui`.
- Broad tests when behavior may cross crates: `cargo test --workspace`.
- Clippy for local quality/Sonar input: `cargo clippy --workspace --all-targets`.
- Refactor dependency guardrails: `scripts/check-refactor-guardrails.py`.
- Local pre-PR path: `scripts/pre-pr-checks.sh` (runs `cargo metadata --no-deps --format-version 1`, guardrails, `cargo fmt --all --check`, `cargo check --workspace`, `cargo clippy --workspace --all-targets`).
- Format check: `cargo fmt --all --check`; apply formatting with `cargo fmt --all` when editing Rust.
- Seek diagnostics local trace: `RUST_LOG=player_core=debug,symphonia_demux=debug,app_egui=debug cargo run -p app-egui -- /path/to/media.webm`.
- Seek diagnostics parser: `scripts/parse-seek-diagnostics.py --scenario "<name>" /tmp/rustiplayer-seek.log`; supports `--format csv` and `--format json`.
- Local Sonar scan only on explicit request and with token in env, not files/history: `SONAR_USER_HOME=/tmp/rustiplayer-sonar-user-home scripts/sonar-local-analysis.sh`.
- After Sonar scanner success, wait for the printed `/api/ce/task?id=...` background task before reading issues.