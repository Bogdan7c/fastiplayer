# MPEG-TS Demux Core

## S29 production contract (2026-07-22)

- `crates/mpeg-ts-demux` — first-party container owner без Symphonia/FFmpeg/HLS/network dependencies. Он реализует только S00 profile-required 188-byte MPEG-TS; 192-byte M2TS явно unsupported до отдельного evidence.
- Factory identity: container `mpeg-ts`, extension `ts`, MIME `video/mp2t`; supported neutral inputs — seekable `ByteSource`, sequential byte stream и ordered segments. Probe использует bounded signature/resync и не доверяет extension сильнее content.
- Parser modules владеют framing/resync, PSI PAT/PMT+CRC/version, fail-closed single playable program selection, per-PID continuity/PES, independent 33-bit PTS/DTS unwrap, PCR (включая отдельный PCR PID), elementary packetization, video AU assembly и VOD index. Gap/TEI/discontinuity сбрасывает только affected PID; corruption не превращается в EOF; scrambling — typed fatal.
- Supported elementary types: H.264 0x1b, H.265 0x24, AAC/ADTS 0x0f, MPEG audio 0x03/0x04 с frame-header classification MP1/MP2/MP3. LATM/private 0x06/AC-3/E-AC-3/subtitles/SCTE вне profile.
- Video AU assembler bounded через typed `video_access_unit_bytes`, сохраняет AU/config/VCL через PES boundaries и публикует только complete AU. H.264/H.265 parsing/keyframe/config semantics reuse `codec-core`; tracks advertise `media_core::VideoPacketFraming::AnnexB`.
- Lifecycle: PMT/config/in-band или ordered discontinuity публикует `TracksChanged` до зависимого packet. `OrderedSegmentDiscontinuity::StartsNewTimeline` — explicit transport→demux boundary.
- Seekable local input строит capped sparse PCR/keyframe index ограниченным initial window; seek может расширить его ровно одним bounded cancellation-aware on-demand window. Scan error/cancel transactional rollback-ит reader/parser playback state; successful partial progress сохраняет continuation. Повторный seek может продолжить; uncovered target возвращает typed unavailable.
- App local-file production composition живёт в `app-egui::local_media`: один уже открытый `LocalFileSource` передаётся app-owned registry с Symphonia + MPEG-TS factories. Signature открывает `.ts`, extensionless и conflicting-extension TS; source cancellation, fingerprint и final revalidation остаются one-handle/typed. Web/HLS/network composition не входит в S29.
- Neutral framing migration: `VideoPacketFraming::{Unspecified, AnnexB, LengthPrefixedFromCodecConfiguration}` принадлежит `media-core`; player-core переводит evidence в codec-core packetization. Symphonia маркирует avcC/hvcC rows length-prefixed; container guessing и fake hvcC запрещены.
- Hermetic generated fixture suite покрывает muxed/audio-only, H.264/H.265, AAC/MPEG audio, arbitrary chunks, CRC/version/multi-program, gaps/duplicates/TEI/scrambling/resync, PTS/DTS/PCR rollover, AU/PES splits, config lifecycle, ordered/in-band discontinuity, bounded/cancelled seek/index and local prepare/rebuild. No checked-in real media fixture.
- Проверки S29: 35 mpeg-ts tests; media-core 36; demux-api 27; player-core 571; symphonia-demux 163; focused app local tests; strict Clippy; Rust 1.96/1.92 workspace checks; fmt/diff/refactor guardrails. Dependency report keeps pre-existing unmaintained advisories for audiopus_sys/ttf-parser; S29 adds no external runtime dependency.


## Ordered-segment PES/AU flush invariant (2026-08-05)

- На каждом новом ordered TS resource предыдущий segment сначала проходит тот же strict PES/AU finalization, что clean EOF, и только затем очищается transport-local continuity/PAT/PMT/PES state. Это не даёт continuity restart-у молча уничтожить последний video RAP segment-а.
- Finalization остаётся fail-closed: incomplete declared PES возвращает typed `MpegTsDemuxError::Malformed` до публикации video packet-а; повреждение не превращается в clean EOF. Explicit discontinuity не склеивает соседние epochs: предыдущий flush завершается до reset/lifecycle следующей timeline.
- Focused owner proof требует оба RAP (0s и 1s) от двух independent ordered segments со стабильными tracks и отдельно проверяет rejection truncated PES без packet publication.
- HLS grouped VOD использует этот owner boundary для decode-safe tail restart; HLS не пытается самостоятельно собирать PES/AU.
- Проверка 2026-08-05: `cargo test -p mpeg-ts-demux` — 37/37; affected all-target Clippy `-D warnings` и Serena diagnostics — PASS.
