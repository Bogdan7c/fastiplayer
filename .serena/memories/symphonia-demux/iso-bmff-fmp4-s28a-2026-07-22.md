# ISO BMFF/fMP4 S28A (2026-07-22)

## Scope и ownership

- S28A завершает finite VOD proof для classic MP4/M4A/MOV/3GP и W3C ISO BMFF byte-stream minimum для fragmented MP4/CMAF. HLS/DASH/ISM manifests, live/DVR, repeated init/discontinuity и `avc3` остаются S31+ / отдельным расширением.
- `demux-api` владеет neutral inputs, exact per-container capabilities, bounded sniff/replay и registry selection. `symphonia-demux` владеет adapter-ом и neutral events. `symphonia-format-isomp4-patch` остаётся единственным ISO box/timing owner; новый parser не добавлен. Codec-private/packetization остаются в `codec-core`.
- Human evidence: `docs/iso-bmff-s28a1.md` и `docs/iso-bmff-fmp4-s28a3.md`.

## Classic proof

- `crates/web-media-http/tests/progressive_containers.rs` проводит один generated MP4/M4A corpus через local `LocalFileSource`, HTTP Range 206 и non-Range 200; проверяет seekability, duration, codec private, packet bytes, PTS/DTS/duration и seek. MOV/3GP brand families, sniff without extension и signature-over-conflicting-hint закреплены отдельно.
- Truncated/malformed/cancel evidence остаётся на factory/local-probe/parser/progressive boundaries. Existing ctts/stss, MP4 color/HDR, orientation и MOV PCM regressions сохранены.

## Finite ordered boundary

- Только ISO BMFF registration имеет `OrderedSegments`; соседние containers сохраняют seekable/streaming only. Factory/planner используют exact row capabilities.
- `OrderedSegmentReader` удерживает только current segment. Lifecycle: ровно один non-empty Init первым, затем non-empty Media с strictly increasing sequence (gaps допустимы). Media-before-init, repeated init, missing init, empty, duplicate/decrease, cancellation и source failure typed; eager Symphonia probe не скрывает concrete error.
- Ordered demux всегда `NotSeekable`; zero/unknown duration нормализуется только внутри ordered wrapper, generic track mapping не меняется.

## Fragmented parser invariants

- Каждый `traf` требует ровно один `tfdt`; v0/v1 `baseMediaDecodeTime` задаёт абсолютный first DTS и сохраняет gaps/non-zero starts. Packet order остаётся container/DTS; PTS = DTS + existing signed/unsigned `trun` composition offset.
- Effective flags precedence: per-sample `trun` → first-sample flags → `tfhd` default → `trex` default. Только video handler требует proven RAP: non-sync bit clear и `sample_depends_on == 2`; unknown/dependent/reserved не угадываются. Audio/non-video uses timestamp candidate.
- Fragmented seekable local/Range seek откатывается к ближайшему proven video RAP at/before target; classic `stss` path не изменён.
- Fragmented zero mvhd/mdhd without authoritative non-zero header, `mehd` or existing `sidx` yields unknown duration, never fake zero.
- Clean EOF допустим только на box boundary. Partial header/declared box/moof/traf/trun/mdat/sample payload maps to structural parse error inside the patch; generic symphonia-demux UnexpectedEof compatibility fallback remains unchanged.
- Upstream `first_sample_flags` duration/size misinterpretation corrected: flags affect only flags, never default sample duration/size.

## Focused verification

- Direct patch: `cargo test --manifest-path crates/symphonia-format-isomp4-patch/Cargo.toml --locked` (40 tests at completion).
- Main dependent proof: `cargo +1.96.0 test --locked -p demux-api -p symphonia-demux -p web-media-http -p codec-core -p audio`.
- Ordered/fMP4 integrations: `crates/symphonia-demux/src/factory/tests/{ordered_segments,fragmented_isomp4}.rs`.
- Patch inventory `docs/dependency-patches.toml` owns atoms/mod, moof, tfdt, traf, trun, demuxer and stream changes; run `scripts/ci-checks.sh dependency-patches`.
