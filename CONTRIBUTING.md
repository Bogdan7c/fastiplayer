# Contributing to Fastiplayer

Fastiplayer is Linux-first software in active development / pre-alpha. Reproducible bug reports, focused fixes, consumer-level tests, and clear documentation are welcome. One core maintainer reviews contributions; response times are not guaranteed.

## Choose work and the right channel

1. Read the [current limitations and ordered roadmap](README.md#current-limitations), then search existing issues and pull requests before opening another one.
2. Report broken behavior through the [bug report form](https://github.com/Bogdan7c/fastiplayer/issues/new?template=bug_report.yml). Include a small, shareable reproduction and the environment that actually failed.
3. Use the [feature request form](https://github.com/Bogdan7c/fastiplayer/issues/new?template=feature_request.yml) for a concrete proposal with a user problem, expected behavior, and scope. General questions and exploratory ideas belong in [Discussions](https://github.com/Bogdan7c/fastiplayer/discussions); see [SUPPORT.md](SUPPORT.md) for routing.
4. Prefer a bounded issue with clear expected behavior and an identifiable owning module. Confirm scope with the maintainer before a substantial feature or architectural change. A large roadmap issue is not automatically a beginner task; `good first issue` is appropriate only for an actually scoped small task. No such issue or assignment is promised by this guide.
5. For suspected vulnerabilities, follow [SECURITY.md](SECURITY.md); do not use a public issue or pull request to disclose them.

## Clone and build on Linux

Install Git, rustup, and a C/C++ toolchain. The repository selects Rust **1.96.0**, rustfmt, and Clippy through [rust-toolchain.toml](rust-toolchain.toml). Rust **1.92.0** is the separately checked MSRV.

Ubuntu 24.04 build and test prerequisites, matching the [CI dependency policy](docs/continuous-integration.md):

```bash
sudo apt-get update
sudo apt-get install build-essential clang libclang-dev pkg-config \
  libasound2-dev libavcodec-dev libavutil-dev libdrm-dev libgbm-dev libva-dev \
  libsoundtouch-dev libvulkan1 mesa-vulkan-drivers
```

These package names describe the CI environment; they are not a claim that every Linux distribution uses them. Actual playback also needs a graphical session, a working Vulkan renderer, audio output, and the appropriate VA-API driver for hardware decode.

```bash
git clone https://github.com/Bogdan7c/fastiplayer.git
cd fastiplayer
cargo build -p app-egui --release --locked
```

The executable is `target/release/fastiplayer`. Before public launch, cloning requires repository access. Public unauthenticated cloning is a launch verification item. System `yt-dlp` is optional for supported web-page extraction; native direct sources do not require it. See the [media compatibility matrix](docs/web-media-compatibility-matrix.md) for its accepted version and profiles.

## Ordinary contributor checks

Run these from the repository root:

```bash
cargo check --workspace --all-features --locked
cargo test --workspace --all-features --locked
scripts/ci-checks.sh format-guardrails
scripts/ci-checks.sh clippy
scripts/ci-checks.sh docs
cargo check -p app-egui --no-default-features --locked
```

Workspace tests need the native test dependencies above and a headless Vulkan adapter (Mesa lavapipe is used in CI). They include tests that reach rendered-frame readback or nonzero PCM, but do not qualify your physical display, VA-API driver, or speakers. Some tests create loopback servers; a sandbox that forbids sockets must allow the test process to bind locally. Do not change production behavior to accommodate a sandbox restriction.

Command verification during S07 (2026-09-05): the clone command succeeded with existing maintainer access and fetched the same revision as the working checkout; the release build and all six ordinary checks above passed in the working checkout. This was not an anonymous public clone or a clean Ubuntu installation test. Build artifacts/dependencies were already available locally. The all-features tests were run with loopback sockets allowed; hardware acceptance was not rerun for these documentation changes.

The complete pre-PR workflow is described in [CI](docs/continuous-integration.md): `scripts/pre-pr-checks.sh` delegates to `scripts/ci-checks.sh all`, adding dependency policy, all seven standalone upstream patch suites, patch integration, and MSRV. It needs Python 3, Rust 1.92.0, cargo-deny 0.20.2, and cargo-machete 0.9.2 in addition to the primary toolchain. Coverage has a separate [qualification workflow](docs/code-coverage.md); never hand-edit its machine-generated baseline to make a change pass.

For a small change, first run the affected crate's functional tests, then the relevant gates and neighboring consumers. Explain skipped checks in the PR. Documentation-only changes require format, link, and command validation; rerunning hardware acceptance does not make a documentation edit more correct.

## Hardware acceptance is separate

Use the [manual media regressions](docs/manual-media-regressions.md), [runtime acceptance manifest](docs/runtime-acceptance-manifest.md), and [manual hardware workflow](.github/workflows/hardware-acceptance.yml) when a change affects real decoding, GPU import, presentation, timing, or device output.

Select your own authorized media inputs explicitly. Record the revision, OS, CPU/GPU, driver/Mesa/libva versions, codec/profile, decoder path, rendering path, and actual outcome. A decoder producing packets or frames is insufficient: successful video acceptance must reach rendering/presentation, and audio must reach the relevant consumer. Distinguish PASS, FAIL, NOT RUN, and profile exclusions. Never treat unavailable hardware or an unselected fixture as a pass. Keep private media, URLs, and unreviewed logs out of the repository.

The existing [N15 report](docs/native-web-ingress-n15-acceptance.md) applies only to its recorded machines and profiles. Native HDR display output and T480s qualification must not be inferred from headless tests or HDR-to-SDR results.

## Architecture and change boundaries

| State owner | Responsibility / boundary |
| --- | --- |
| `app-egui` | UI and composition; translates user intent into source/session operations |
| `player-core` / `PlaybackPipeline` | Playback scheduling and resource lifecycle through intent methods; preserves generation, backpressure and release semantics |
| Source, protocol and demux crates | Bounded transport, manifests, source recovery, packets and timing |
| `video-backend-api`, `video-frame-contract` | Backend-neutral decode and exact frame representation/lifetime contracts |
| `video-vaapi`, `video-ffmpeg` | Concrete hardware/software decoding behind those contracts |
| `render-wgpu-video`, `render-wgpu-shell` | GPU conversion/tone mapping, frame consumption, surface composition and presentation |
| Audio, playlist and settings owners | Audio clock/output, durable queue identity, validated settings and transactional apply/rollback |

Read [ARCHITECTURE.md](ARCHITECTURE.md) and the [engineering rules](AGENTS.md) before editing. Describe owners, boundary methods, invariants, and tests before a new feature. Keep state in its owning module, use intention-revealing APIs, preserve distinct error/absent/backpressure outcomes, and avoid reaching into another module's fields. Keep architectural refactors separate from feature and cosmetic changes. New features normally belong in a separate module when the existing file approaches 700–800 lines.

Tests must verify observable working functionality and regressions at the consumer boundary. For a new boundary cover an absent resource, an active fake/stub, errors, accounting edges, and state the boundary must not own. Explain key production logic, non-obvious decisions, and invariants in Russian comments; do not annotate every line. Public contribution documents are English; the [index](docs/README.md) labels the deeper documents' languages.

## Prepare a pull request

Keep changes focused. Explain the concrete problem, resulting behavior, related issue, validation results, and remaining limitations using the PR template. Self-review the diff and local links; remove sensitive logs, credentials, private URLs, and media you cannot redistribute. Preserve upstream patch licenses and notices; first-party contributions use the project's [MIT license](LICENSE).

AI tools are optional, not build or contribution prerequisites. If using an agent, follow [AGENTS.md](AGENTS.md) and the [AI workflow](docs/ai-development.md), including Context7 and Serena instructions. The contributor remains responsible for understanding, reviewing, and testing the result.

Be respectful and keep technical disagreement focused on the work. A formal Code of Conduct is deliberately deferred until a separate private enforcement contact exists; the vulnerability reporting channel is not a substitute for that contact. See [MAINTAINERS.md](MAINTAINERS.md).
