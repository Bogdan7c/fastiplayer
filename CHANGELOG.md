# Changelog

Notable project changes are recorded here in a Keep a Changelog-like structure. Development prereleases may change behavior and interfaces; this is not a stable compatibility guarantee. Earlier private development is preserved in Git history rather than reconstructed as fictional public releases.

## [Unreleased]

### Added

- Drag-and-drop of files, folders, playlists, and links on Wayland and X11 (roadmap item 7), backed by an additive [winit patch](docs/dependency-patches.toml).
- Opening a file while Fastiplayer is running forwards it to the existing window over D-Bus instead of refusing to start.
- Volume from the mouse wheel and arrow keys, playback-speed hotkeys, Ctrl+O, a timeline time hint; volume and mute persist between launches.
- Audio-only HLS (internet radio), including packed ADTS AAC/MP3 segments; video-only HLS; finished EVENT playlists open as VOD; VP9 in fMP4 HLS.
- `.m3u`/`.cue` playlists in Windows-1251 are read with a notice; playlist export reports skipped entries.
- A "Retry" action for open errors; URL import shows progress, cancellation, and a clear rejection reason.

### Changed

- User-facing errors explain the cause in plain (currently Russian) text instead of internal messages: missing files, broken media, network and web-page failures, settings rebuilds.
- A broken queue item is skipped with a visible reason; a queue without network stops on the first item instead of failing through it.
- A broken config no longer prevents startup: it is backed up to `.bak` and defaults are used with a warning.
- A lost audio device falls back to the default output.
- Upgraded to Rust 1.99, egui 0.36, and wgpu 30.

### Fixed

- Settings Apply that rebuilds media no longer freezes the window.
- Stop during source preparation cancels opening quietly, without an error.
- Pausing before the first frame shows a poster frame instead of a black screen.
- Live HLS starts behind the live edge, removing buffering cycles at the edge.
- Recreating the audio output no longer shifts audio against video.
- VA-API AV1 global-motion parsing no longer hangs AMD VCN; AV1/VP9 frame presentation times follow the shown temporal unit.

### Planned

- Continue the ordered [roadmap to 1.0](README.md#roadmap-to-10); roadmap entries are not shipped features.

## [0.1.0-alpha.1]

First public development release scope, with tag name `v0.1.0-alpha.1`: Linux-first, source-only, active development / pre-alpha. Publication status and generated source archives are available on the [Releases page](https://github.com/Bogdan7c/fastiplayer/releases); no portable binaries are provided. Workspace packages and their internal version requirements are aligned to `0.1.0-alpha.1`; the Cargo lockfile is updated with Cargo.

### Added

- Initial public release scope: local playback, VA-API hardware and FFmpeg software video decode, and WGPU/Vulkan rendering within supported profiles.
- GPU HDR-to-SDR tone mapping, audio playback, seeking, stream selection, playlists, and runtime settings.
- Native progressive HTTP(S)/FTP(S), HLS and DASH VOD/live/DVR, and supported static Smooth Streaming/HDS profiles; optional system `yt-dlp` for web pages.
- Public architecture/contribution/security/support documentation, maintainer ownership, and GitHub issue/PR templates.

### Release preparation

- Integrated the real T480s runtime screenshot and separately scoped hardware/software CPU/RSS evidence, preserving measured source/binary identity and raw samples.
- Kept N15 ingress measurements separate and replaced removed owner-local acceptance fixture references with explicit placeholders and local setup instructions.
- S08 fixed media-install snapshot publication ordering, near-forward prefetch seek races, and XWayland fullscreen surface resizing before the recorded measurements.

### Known limitations

- Vulkan remains required for rendering; hardware decode depends on the GPU, driver, codec/profile, and frame-import compatibility.
- Native HDR display output, a native subtitle engine, and native Windows application support are not implemented. macOS is outside the roadmap through 1.0.
- Streaming support is profile-limited; DRM and several protocol variants are unsupported. The UI contains Russian text and remains under development.
- T480s results describe the recorded S08 binary and specific workloads, not a new alpha measurement or perfect 60 FPS. AV1 is software-only on this machine; equivalent AV1 VLC output was not established. N15 ingress remains a separate experiment.

See the [release notes](docs/releases/v0.1.0-alpha.1.md), [accepted profiles](docs/web-media-compatibility-matrix.md), and [N15 evidence and limitations](docs/native-web-ingress-n15-acceptance.md) for the exact scope.
