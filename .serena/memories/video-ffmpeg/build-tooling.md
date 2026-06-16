# Video FFmpeg Build Tooling

- New preparatory workflow added in `scripts/tooling/build-ffmpeg-lgpl.sh` with docs in `scripts/tooling/README.md` and a pointer from `docs/rustiplayer/README.md`.
- Purpose: build local dynamic LGPL FFmpeg 8.1.x libav* for future software-decode experiments only. This does not add `video-ffmpeg` to the workspace, does not add FFmpeg/libav crates to Cargo manifests, and does not make runtime startup depend on FFmpeg.
- Default FFmpeg version is `8.1.1` (stable 8.1.x); the script rejects non-8.1.x versions until a separate architecture decision updates the baseline.
- Default prefix is `target/rustiplayer-ffmpeg/<version>`; override with `--prefix` or `RUSTIPLAYER_FFMPEG_PREFIX`. Build/cache dir defaults to `target/rustiplayer-ffmpeg/build`.
- Source inputs: default official URL `https://ffmpeg.org/releases/ffmpeg-<version>.tar.xz`, or `--source-archive`/`RUSTIPLAYER_FFMPEG_SOURCE_ARCHIVE`, or `--source-dir`/`RUSTIPLAYER_FFMPEG_SOURCE_DIR`.
- Configure policy: `--enable-shared`, `--disable-static`, `--disable-programs`, `--disable-doc`, explicit `--disable-gpl`, `--disable-nonfree`, `--disable-autodetect`, required `libavcodec` + `libavutil`, disabled `libavformat`/`libavdevice`/`libavfilter`/`libpostproc`, disabled hwaccels/encoders/muxers/demuxers/protocols/devices/filters.
- `libswresample` and `libswscale` are disabled by default; they can be enabled only via explicit CLI/env (`--enable-swresample`, `--enable-swscale`, `RUSTIPLAYER_FFMPEG_ENABLE_*`) for future header/build probes, not for CPU playback conversion.
- Script contracts checked in Session 1: `bash -n`, `--help`, default `--dry-run`, opt-in swresample/swscale dry-run, non-8.1.x rejection, `scripts/check-refactor-guardrails.py`, and `cargo check --workspace` without FFmpeg env/prefix.
