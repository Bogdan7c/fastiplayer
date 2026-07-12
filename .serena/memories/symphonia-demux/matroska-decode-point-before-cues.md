# Matroska/WebM DecodePointBefore cue anchors

`symphonia-demux` keeps public/player-core Accurate seek semantics unchanged: `DemuxSeekRequest::DecodePointBefore(target)` still reports the user target as requested position, and accepted playback/timeline commit remains target-position at higher layers.

For Matroska/WebM only, `SymphoniaDemuxer` now pre-scans bounded `Cues` from seekable files/byte sources and stores a `MatroskaCueIndex`. The index lives in `matroska_metadata.rs` and is fail-open: missing, incomplete, invalid, or unreadable cues return an empty index and the old generic retry path is used.

Initial `DecodePointBefore` backend seek for selected video track uses the nearest Matroska video cue `<= target - 1ms`. Strict packet verification is not weakened: first accepted selected video packet must still be not after target, within accepted preroll, and not proven non-keyframe.

Symphonia Matroska can physically land before the chosen cue and then forward to a non-keyframe/after-target packet. For that case, Matroska retry can seek to the previous cue before the generic 5s backoff, while carrying `minimum_video_timestamp` so verification skips packets earlier than the logical cue and does not buffer/replay pre-logical-cue audio/video. Do not use previous-cue retry for `FirstVideoTooFarBeforeTarget`; that case must go to the existing rescue retry closer to target.

Focused coverage: unit tests in `symphonia_demuxer.rs` cover cue anchor selection, previous-cue retry, too-far rescue, and the bounded startup-keyframe exception. Fixture tests in `tests/h264_fixtures.rs` and `tests/h265_fixtures.rs` cover MKV targets 0s/3s/5s/10s and assert a near cue/keyframe instead of old 5s backoff. Existing MOV/MP4 and VP9/WebM fixture tests should remain green.

For seekable sources, a Symphonia `OutOfRange` specifically from `DecodePointBefore(0)` is handled by rebuilding the `FormatReader` at the physical source start and then running the existing packet-level keyframe/startup-lead verification. This fixes fresh-media backend reselection at timeline zero for Matroska files whose first cluster/track timestamp starts slightly after zero. The fallback must not apply to Accurate/Preview, non-zero targets, unseekable sources, or non-OutOfRange failures.