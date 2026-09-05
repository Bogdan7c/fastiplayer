# HLS Previewed → Receipted cancellation (2026-08-27)

## Root cause and boundary

`ProgressiveSeekCommand::Previewed` previously had no `DemuxSeekCancellationToken`, so the single progressive worker could remain blocked inside HLS seek while a newer final receipt waited. The additive typed `Demuxer::seek_with_cancellable_preview_request` preserves preview semantics while allowing cooperative in-flight cancellation. Default behavior checks pre-cancel and delegates to preview-compatible `seek_with_request`.

Progressive commands own per-request tokens. Supersede cancels active/pending preview or receipt, and the worker calls the matching cancellable boundary.

## HLS lifecycle

HLS factories clone the same seek token into offside sources. Component and separate A/V replacements prepare away from committed state. A cancellable transaction wins `complete()` before active-read activation, final component/composite assembly, staged index/diagnostic evidence, and the single active-state swap. If cancellation wins, the old committed source/pair remains authoritative.

A newly packet-proven manifest anchor is no longer inserted into the shared preview index during offside prepare. It is staged and inserted only in the authorized commit section. Dropping staged evidence leaves the index unchanged.

Separate A/V uses one shared token for video and audio. Both sources are activated only after commit authorization; both staged selections are committed together immediately before the composite swap.

Committed selection evidence публикуется через neutral `log` facade как INFO target `fastiplayer::hls_manifest_selection`. Marker содержит только HLS-owned safe scalars; concrete backend остаётся в composition root. Публикация остаётся внутри authorized staged commit, поэтому cancellation/failure/stale path не создаёт marker.

## AES media/key physical cancellation

Encrypted media and external keys no longer use uncancellable `fetch_resource_blocking`. HLS owns `fetch_cancellable_full_resource`, which:

- accepts the already policy-complete typed request (redirect/status/range/max-body/secret forwarding semantics remain in adaptive transport);
- opens the existing streaming boundary with the same seek token;
- optionally registers the existing restartable active-read attempt for media/init, but not for external keys;
- consumes chunks to EOF under the transport body bound;
- records transport/expiry errors without turning cancellation into EOF;
- returns bytes only after complete EOF, so partial key bytes never enter `SharedHlsKeyCache`.

Dropping/cancelling the streaming body physically releases the response and prevents stale plaintext/key/packet publication.

## Functional coverage

- media-core: default delegation, pre-cancel no mutation, preview/receipt semantic split.
- demux-api: causal worker test proves final receipt starts after cancelling in-flight preview without manual release.
- HLS plaintext loopback: partial preview TCP closes, final starts and lands across discontinuity.
- HLS AES loopback: partial ciphertext and partial rotated external key each close the old TCP; final starts before release, succeeds and publishes only final packets. Partial key is not cached.
- HLS separate A/V loopback: independent partial video and partial audio candidate bodies are cancelled physically. A deliberately failed pre-commit receipt then proves the old committed pair is still non-EOF/readable from exact video/audio tail packets with stable public track IDs and no `TracksChanged`; a separate successful receipt publishes exactly one topology commit and no stale preview packet. The fixture deliberately bounds the progressive queue to two events: queue capacity plus one in-flight stale push cannot consume the `75s` proof tail after the `70s` anchor. Ordering assertions apply only until each component's exact tail is observed, because valid later packets from that component may interleave while the peer tail is still pending.
- Shared index unit tests prove drop does not mutate and authorized commit does.

## Verified commands

- `cargo test -p media-core -p demux-api -p web-media-hls --all-targets --all-features`
- `cargo test -p web-media-hls --all-targets --all-features`
- new AES and separate-A/V focused integration tests
- grouped-TS exact flaky test 10/10
- `cargo clippy -p media-core -p demux-api -p web-media-hls --all-targets --all-features -- -D warnings`
- `cargo fmt -p web-media-hls -- --check`
- `git diff --check`
- `scripts/check-refactor-guardrails.py`

## Confirmed real release acceptance (2026-08-28)

- Clean committed HEAD `72a3cbf7` release matrix passed cold InitialOpen/InitialRestore, 10 warm final seeks, 3 restart seeks, 3 causal rapid `550 -> 60` pairs and 3 actual KWin EIS timeline drags.
- Rapid setup waited until each old 550 request entered worker/HTTP. Old requests then physically terminated cancelled in `7/8/11 ms` with no old receipt/frame/audio/commit/progress; each winning 60 completed in `28 ms`.
- Drags produced `4/7/7` preview dispatches. Cancelled candidates emitted no committed marker; final receipts completed with video/audio/UI progress.
- Strict aggregate HLS/network/scrub proof anomalies were 0. Proven silence-padding underrun markers were 0; three risk-only drag observations had zero new silence-padding callback delta.
- Real x36 playlist contained no discontinuity; sequence remained 0. Discontinuity/AES/key/separate-A/V atomic cancellation claims therefore rely on the hermetic loopback tests above, not on the CDN manifest.
- One warm `1169 ms` external body-delivery residual occurred before receipt; receipt-to-video/audio/commit was `18/19/19 ms`. It does not weaken cancellation correctness and is not a durable latency promise.
- Final HLS change is committed; worktree after acceptance was clean.

Related: `mem:demux-api/core`, `mem:media-services/hls-vod-manifest-receipted-seek-2026-08-24`, `mem:media-services/hls-manifest-selection-diagnostics-2026-08-27`.
