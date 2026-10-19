# Changelog

Notable project changes are recorded here in a Keep a Changelog-like structure. Development prereleases may change behavior and interfaces; this is not a stable compatibility guarantee. Earlier private development is preserved in Git history rather than reconstructed as fictional public releases.

## [Unreleased]

Planned as 0.2.0.

### Added

- Native Rust subtitle engine (roadmap item 1, [issue #1](https://github.com/Bogdan7c/fastiplayer/issues/1)) drawn by its own GPU pass between the video and the interface. Its exact limits are in the [subtitle compatibility matrix](docs/subtitles/compatibility.md).
  - Formats: SRT, WebVTT, ASS/SSA (a port of libass 0.17.5 behaviour, checked against libass reference images), PGS inside MKV, MP4 `mov_text`, YouTube srv3, TTML/IMSC.
  - Sources: files next to the video, dropped and loaded files, embedded MKV/MP4 tracks with attached fonts, YouTube manual / automatic / auto-translated captions through yt-dlp, HLS WebVTT and DASH text tracks.
  - Controls: CC button and menu, an audio-track menu, hotkeys `C` / `V` / `Z` / `X` / `B`, auto-selection by preferred languages with forced tracks, sync delay.
  - Look: on-video appearance editing; subtitles rise above the control panel.
  - Memory: track, Off and delay choices are remembered per video. See the [user guide](docs/subtitles/user-guide.md).
- The software decoder no longer depends on the system FFmpeg. The build downloads FFmpeg 8.1.3 and dav1d 1.5.4, checks them against pinned SHA-256 sums, and builds a minimal LGPL copy next to the player in `target/<profile>/lib/`. Machines without VA-API, NVIDIA among them, can always play H.264, H.265, VP8, VP9, and AV1 in software. Packagers can link the system FFmpeg 8.1+ with `--no-default-features --features system-ffmpeg`.
- Build instructions for Fedora and RHEL-based distributions.
- YouTube videos with dubbed audio list their audio languages in the audio menu.
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
- New installations start playing an opened file immediately and hide the diagnostic telemetry panel; existing configs keep their saved values.
- Upgraded to Rust 1.99, egui 0.36, and wgpu 30.
- Building now needs `meson`, `nasm`, `ninja`, and `curl` instead of FFmpeg development packages.

### Fixed

- MP4 and fragmented MP4 (local files, HLS fMP4, DASH) honour the common ISO BMFF edit lists written by ffmpeg: a leading empty edit (audio that starts later) and a start shift (B-frame video). Audio no longer runs up to ~0.3 s ahead of the picture on such files.
- Native DASH opens static MPDs whose audio timeline starts slightly after the Period start or ends slightly after it (typical `ffmpeg -f dash` output); it used to fail with an internal "component selection shape" error. DASH streams that offer only video or only audio no longer fail the same way.
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
