# AUD-003 PTS-only packet time-base fix (2026-08-23)

## Root cause and proof

- Separate real-runtime verification used generated seekable MPEG-TS H.264 Constrained Baseline, no B-frames, 5 fps, 20 frames, MPEG clock `1/90000`.
- Before the fix, production MPEG-TS demux emitted growing `track_pts=0/18000/36000` with `track_dts=None`, but player-core and `video_core::DecodePacket` dropped raw PTS.
- Software FFmpeg therefore received `AV_NOPTS_VALUE` PTS/DTS, `AVPacket.time_base=0/1`, `AVCodecContext.pkt_timebase=0/1`; materialized frame PTS were `0/0/0 us`, and after seek to 2 s they were `2.0/2.0/2.0 s`.

## Stable boundaries

- `media_core::Packet` remains demux owner of signed `track_pts`/`track_dts` and their `TimeBase`.
- `player-core::PendingVideoPacketTimestamps` groups media PTS/DTS plus raw track PTS/DTS with named fields. `PendingVideoPacket` carries `track_pts` through queue/backlog-recovery ownership.
- Neutral `video_core::DecodePacket` now includes `track_pts: Option<TrackTimestamp>`.
- `video-ffmpeg::send_receive` selects packet time base from coherent current-track raw PTS first, then raw DTS, then cached stream time base. Exact raw units are used only when track owner/time base agree; otherwise media `Duration` is rescaled into the selected time base.
- Safe FFmpeg FFI owns `PacketTimeBase`, validates positive u32 components fit FFmpeg i32 `AVRational`, sets both `AVPacket.time_base` and `AVCodecContext.pkt_timebase` before send, and keeps raw FFmpeg types private.
- `FramePtsResolver` observes the same PTS-or-DTS time base.
- VA-API intentionally keeps its old media-`Duration` PTS semantics. Its internal queued packet does not retain raw `track_pts`; adapter coverage locks this ownership decision and prevents queue-size regression.

## Regression coverage and workflow

- Focused player tests:
  - `route_demuxed_video_packet_preserves_shared_payload_keyframe_and_pts`
  - `pending_video_packet_preserves_track_timestamps_through_decode_boundary`
- Focused FFmpeg tests:
  - `pts_only_packet_time_base_resolves_materialized_frame_timestamp`
  - `packet_time_base_rejects_values_outside_ffmpeg_rational_contract`
  - `owned_packet_preserves_validated_time_base_and_timestamps`
- Real ignored functional regression: `crates/video-ffmpeg/tests/pts_only_mpeg_ts.rs`.
- Generate the local fixture and run through `scripts/media-regression.sh --scenario h264-ts-pts-only-ffmpeg --path <asset>`; exact generation command is documented in `docs/manual-media-regressions.md`. No real fixture is checked into Git.
- Passing evidence on the confirming asset: start first PTS `0/200000/400000 us`; middle-seek first PTS `2000000/2200000/2400000 us`, target `2000000 us`; current generations, AVFrame-backed handles, normal release, and terminal EOF drain all pass.
- Original audit record updated in `user/project_health_audit_2026-08-22.md`.

## Verification

- `cargo +1.96.0 test -p video-ffmpeg --features ffmpeg --locked --lib`: 86 passed.
- `cargo +1.96.0 test -p video-ffmpeg --locked`: 61 effective tests passed, runtime-only tests ignored/feature-gated as designed.
- `cargo +1.96.0 test -p video-core -p player-core -p video-vaapi --locked`: 51 + 641 + 144 passed.
- `cargo +1.96.0 clippy -p video-core -p player-core -p video-vaapi -p video-ffmpeg --all-targets --features video-ffmpeg/ffmpeg --locked -- -D warnings`: passed.
- `cargo +1.96.0 check --workspace --locked`, `cargo +1.96.0 fmt --all --check`, `bash -n scripts/media-regression.sh`, and `git diff --check`: passed.
- Real scenario passed with exact output above.
- GUI/WGPU scheduler late-drop/freeze behavior was not separately measured; the materialized frame criterion is fully satisfied.
