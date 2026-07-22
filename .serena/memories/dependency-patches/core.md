# Dependency Patches Core

- Root `Cargo.toml` uses top-level `[replace]` for seven critical local patch crates: `cros-libva:0.0.12`, `cros-codecs:0.0.6`, `symphonia-format-caf:0.6.0`, `symphonia-format-isomp4:0.6.0`, `symphonia-format-mkv:0.6.0`, `symphonia-codec-aac:0.6.0`, and `wayland-scanner:0.31.10`.
- These replacements are not feature toggles. Removing any `[replace]` or doing a large upstream bump changes the playback risk profile and requires an explicit architecture/maintenance decision plus a media regression matrix. Do not mix patch removal/upstream sync with feature work.
- Cargo override semantics checked via Context7/Cargo Book on 2026-06-20: root-level overrides affect dependency resolution transitively. Several local patch crates are not normal workspace members; practical validation usually goes through dependent workspace crates such as `video-vaapi`, `symphonia-demux`, `audio`, and the whole workspace.

## Why Each Patch Is Still Needed

- `symphonia-format-caf`: exact 0.6.0 replacement for S28C forward-only CAF. It stops initial chunk scan at `data` on non-seekable sources while preserving seekable full scan/seek-back, and exact-reads declared fixed/variable packets so structural truncation cannot become a short packet or clean EOS. Stream-friendly CAF must place `desc` and required codec configuration before `data`. Removal gate and tests live in `docs/dependency-patches.toml`; full proof: `mem:symphonia-demux/audio-containers-s28c-2026-07-22`.
- The CAF patch is MPL-2.0, excluded from workspace membership, carries its own lock, and participates in the seven-entry inventory/CI matrix.

- `cros-libva`: local replacement for the cros-libva version pulled by cros-codecs. It carries compatibility with newer system libva headers, libva version cfg/check-cfg handling, VP9 encoder ABI fields, and VA surface status/query paths used by cros-codecs/video-vaapi. Removing it can break build compatibility or decoded-surface readiness semantics.
- `cros-codecs`: production VA-API codec layer for VP9/H.264/H.265. Local needs include H.265 Dolby Vision RPU / unspecified NAL type 48..63 acceptance, dynamic WPP entry point storage for 4K streams, H.265 seek/flush picture-order reset and `NoRaslOutputFlag` behavior for CRA/IRAP, RPS/DPB diagnostics, and `DecodedHandle::try_is_ready()` so VA query errors are not collapsed into `true` during suppressed-surface reclaim.
- `symphonia-format-isomp4`: MP4/fMP4 container patch for composition offsets (`ctts`/`trun`) so B-frames get presentation PTS, classic `stss` and fragmented effective-sample-flags sync-safe video seek, mandatory fragmented `tfdt` v0/v1 decode timing/gaps, honest fragmented duration authority, structural truncation distinct from clean EOF, `tkhd` display orientation tags, MP4 `colr`/`mdcv`/`clli` raw tags for neutral color/HDR metadata, and QuickTime/iOS PCM/LPCM one-frame sample coalescing to avoid tiny-packet starvation after seek. Video unknown flags are never guessed as RAP; audio/non-video fragmented seek uses timestamp landing.
- `symphonia-format-mkv`: exact 0.6.0 Matroska/WebM patch for `CodecState -> ResetRequired` ordering before dependent packets and container-owned structural EBML truncation. It is the single parser owner; details and removal gate: `mem:symphonia-demux/matroska-webm-s28b-2026-07-22`.
- `symphonia-codec-aac`: removes the upstream AAC-LC `channels.count() > 2` complexity guard and owns coded-order → canonical-plane mapping. For indexed `channel_configuration != 0`, coded position/type selects the destination plane; `element_instance_tag` is accepted with any 4-bit value and only participates in duplicate identity validation inside its element-type namespace. For 5.1 coded `FC,FL,FR,RL,RR,LFE` becomes canonical `FL,FR,FC,LFE,RL,RR` (`[2,0,1,4,5,3]`). Config 3–7, arbitrary-tag compatibility and duplicate-tag rejection have direct patch tests; the per-frame cursor also rejects incomplete/extra elements before synthesis. This mapping is required before any downstream layout-aware downmix; removing it silently swaps dialogue/LFE/surround roles.

## Verification Before Removal Or Major Bump

- Run at least `cargo check --workspace` and `cargo clippy --workspace --all-targets`.
- All seven patch directories are explicit `workspace.exclude` entries with their own `Cargo.toml` and `Cargo.lock`. Each must pass `cargo test --manifest-path <patch>/Cargo.toml --locked`; never add them to `workspace.members` or make them inherit first-party workspace metadata/lints.
- `docs/dependency-patches.toml` is the checked machine-readable inventory: crates.io archive SHA-256 upstream identity, reason, owned diff areas, dependents, focused tests, manual media matrix, and removal gate. `scripts/check-dependency-patches.py` validates it against root `[replace]`, manifests, locks, excludes, and the no-membership rule.
- `scripts/ci-checks.sh dependency-patches` runs the checked inventory plus `cargo test -p video-vaapi -p symphonia-demux -p audio --locked`. CI also has seven independent matrix jobs running the exact direct locked test for each patch.
- Run focused tests for dependent crates touched by the patch behavior (`video-vaapi`, `symphonia-demux`, `audio`, plus neighboring crates when contracts move). Direct `cargo test -p cros-codecs-patch`/similar is not currently reliable under the workspace layout; use dependents unless workspace membership is intentionally changed.
- Manual media regression should cover H.264 MP4 with B-frames and seek, H.265/HEVC MOV/MP4 including iOS/Dolby Vision RPU and CRA/open-GOP seek, VP9 SDR/HDR VA-API DMA-BUF export, MP4 HDR/color metadata, QuickTime/iOS LPCM seek/playback, and AAC-LC 5.1 playback.


## Repository dependency policy (Session 05, 2026-07-10)
- Seven local `[replace]` crates are explicitly listed in workspace `exclude` and remain upstream-licensed: cros patches BSD-3-Clause, Symphonia patches MPL-2.0, wayland-scanner Apache-2.0. They must never inherit workspace MIT metadata.
- First-party workspace packages inherit `license = "MIT"`; root `LICENSE` is standard MIT copyright 2026 Bogdan7c.
- `deny.toml` is the blocking cargo-deny policy. MPL-2.0 is allowed only by named Symphonia exceptions. Unknown registries/Git and unlisted licenses fail; Git sources require a separately reviewed pinned revision plus owner/reason/removal criterion.
- `directories 6` was replaced by permissive `etcetera 0.11` because `option-ext` introduced non-inventoried MPL-2.0.


## S04X wayland-scanner advisory closure (2026-07-20)
- S04X added `wayland-scanner:0.31.10` to root `[replace]`; the current root has seven entries after later S28B/S28C format patches. `wayland-scanner:0.31.10` points to `crates/wayland-scanner-patch`, copied from the exact published crates.io archive and kept outside workspace membership with its own lock.
- Owned patch delta is intentionally narrow: manifests select `quick-xml 0.41`, and `src/parse.rs` supplies `XmlVersion::Implicit1_0` to the changed decoding API. No winit/egui/Wayland behavior or generated protocol semantics are intentionally changed.
- This closes RUSTSEC-2026-0194/0195 without a broad window-stack migration or cargo-deny exception. `cargo tree -i quick-xml --workspace --all-features` must contain only quick-xml 0.41 consumers; `cargo deny check advisories` must remain clean.
- Removal gate: replace the local crate only when an upstream released `wayland-scanner` selected by the current window stack depends on a non-vulnerable quick-xml line and the direct patch tests plus full workspace/Wayland smoke pass. Do not retain the patch after that release is adopted.
- Machine-readable provenance, SHA-256, direct tests and manual matrix live in `docs/dependency-patches.toml`; detailed dependency/source/license/MSRV audit is `docs/dependency-audit-s04x-2026-07-20.md`.
