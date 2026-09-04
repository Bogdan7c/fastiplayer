# Changelog

Notable project changes are recorded here in a Keep a Changelog-like structure. Development prereleases may change behavior and interfaces; this is not a stable compatibility guarantee. Earlier private development is preserved in Git history rather than reconstructed as fictional public releases.

## [Unreleased]

### Planned

- Continue the ordered [roadmap to 1.0](README.md#roadmap-to-10); roadmap entries are not shipped features.

## [0.1.0-alpha.1] — Unreleased draft

Planned first public development release, with tag name `v0.1.0-alpha.1`. Linux-first, source-only, active development / pre-alpha. No tag, release date, downloadable binary, or published release is asserted here. Workspace version alignment is a later launch-preparation step.

### Added

- Initial public release scope: local playback, VA-API hardware and FFmpeg software video decode, and WGPU/Vulkan rendering within supported profiles.
- GPU HDR-to-SDR tone mapping, audio playback, seeking, stream selection, playlists, and runtime settings.
- Native progressive HTTP(S)/FTP(S), HLS and DASH VOD/live/DVR, and supported static Smooth Streaming/HDS profiles; optional system `yt-dlp` for web pages.
- Public architecture/contribution/security/support documentation, maintainer ownership, and GitHub issue/PR templates.

### Known limitations

- Vulkan remains required for rendering; hardware decode depends on the GPU, driver, codec/profile, and frame-import compatibility.
- Native HDR display output, a native subtitle engine, and native Windows application support are not implemented. macOS is outside the roadmap through 1.0.
- Streaming support is profile-limited; DRM and several protocol variants are unsupported. The UI contains Russian text and remains under development.
- T480s screenshot/performance qualification is pending. Existing N15 ingress measurements do not establish whole-player or VLC performance.

See the [draft release notes](docs/releases/v0.1.0-alpha.1.md), [accepted profiles](docs/web-media-compatibility-matrix.md), and [N15 evidence and limitations](docs/native-web-ingress-n15-acceptance.md) for the exact scope.
