# N14B cross-protocol seek/switch/queue/restart lifecycle (2026-09-02)

## Outcome

- Добавлен единый focused filter `n14b_lifecycle`: 17 functional tests закрепляют VOD/live seek, Playing/Paused same-item switch, queue Previous/Next/clean EOF, no-false-live-EOF, graceful close/restart/restore, recovery, persistence correlation и stale generation fences.
- Использованы только существующие N14A consumer-reaching fixtures и production decoder/render/audio owners; новый playback/test runtime и новые media fixtures не создавались.
- Каждый успешный media transition снова достигает либо FFmpeg -> HostPlanar WGPU submit/readback/release, либо nonzero PCM; отдельный неизменённый N14A cohort продолжает доказывать production clock advancement. Direct/native attempts работают без extractor; injected adaptive process spies остаются exact 0.

## Matrix and ownership

- HTTP Ogg и FTP Ogg: forward seek, backward seek и reopen снова дают PCM; HTTP seek не повторяет downloaded root, FTP REST path сохраняет monotonic RETR accounting. `yt_dlp.enabled=false` остаётся structural zero-process fence.
- HTTP WebM: explicit drop/close и cold restart дважды достигают decoded VP9 frame и WGPU readback; каждый attempt даёт exact 2-request probe/read cohort.
- HLS VOD: новый отдельный test owner `media_open/web/tests/native_hls_lifecycle_n14b.rs` удерживает existing N14A server/builders и связывает receipted forward/back seek, queue Next/EOF/Previous, TS/fMP4 semantic switch, exact runtime-owner drop, controlled reopen/position restore и stale catalog action rejection. Queue current коммитится только после consumer success.
- DASH VOD, Smooth VOD и HDS VOD: forward/back seek плюс switch/reopen снова достигают frame/PCM. Smooth backward path использует fresh attempt после fixture EOF; HDS open probes считаются attempt-local deltas отдельно от законных fragment reads после seek.
- HLS/DASH live: retained DVR seek достигает consumers, shifted target expiry остаётся typed error, endpoint/root recovery и semantic reopen/switch продолжают playback, а refresh loops fail immediately на ложном `EndOfStream`.
- Existing app owners входят в тот же cohort: Playing/Paused switch сохраняет state/position до exact Installed; queue игнорирует Playing/Draining и потребляет один clean Ended edge; persistence/shutdown сохраняют только exact correlated/settled position и отвергают stale generation/instance/tombstone.

## Root defect and owner fix

- N14B воспроизвёл HLS MPEG-TS DVR receipt, после которого production-like `avcodec_flush_buffers` давал nonzero PCM, но ни одного video frame.
- Причина: HLS live timeline принимал любой MPEG-TS H.264 IDR как decoder-restart anchor, хотя retained segment мог не содержать in-band SPS/PPS. Receipt был успешным, но player после обязательного decoder flush не мог восстановить video.
- `codec-core` теперь публично владеет `probe_h264_packet_in_band_decode_start`: typed Annex-B/AVCC parser требует ordered SPS -> PPS -> IDR внутри одного access unit и сохраняет parse errors отдельно от `false`.
- `web-media-hls::live::HlsLiveComponentDemuxer` применяет этот строгий proof только для H.264 Transport Stream video packets. fMP4 и другие codecs сохраняют прежнюю keyframe policy; обычная `mpeg-ts-demux` keyframe/index semantics не менялась.
- Timeline evidence получает explicit `HlsLiveVideoDecodeStartEvidence::{Proven, NotProven}`; неполный IDR не расширяет seekable DVR range. Добавлена internal dependency `web-media-hls -> codec-core`, без нового external crate.
- Functional regression использует один persistent production FFmpeg/AAC owner, выполняет тот же decoder/audio reset, что player seek, и требует post-receipt WGPU frame + PCM.

## Verification §6.3

PASS:

- ordinary three-run cohort `cargo test -p app-egui --all-features --locked n14b_lifecycle`: 17/17, 3/3 fresh runs; дополнительный post-self-review run 17/17;
- N14A regression cohort `cargo test -p app-egui --all-features --locked n14a_consumer -- --nocapture`: 10/10;
- `cargo test -p codec-core --locked`: 97 unit + 3 fixture tests;
- `cargo test -p mpeg-ts-demux --locked`: 47/47 после подтверждения, что fix не меняет общий TS index;
- `cargo test -p web-media-hls --locked`: all unit/integration/doc-test targets PASS, включая 9 live runtime tests;
- strict `cargo clippy -p codec-core -p web-media-hls -p app-egui --all-targets --all-features --locked -- -D warnings`;
- `cargo check --workspace --all-targets --all-features --locked`;
- `cargo fmt --all -- --check`, `git diff --check`;
- Serena diagnostics clean после refresh.

## Scope and handoff

- Public-network, GUI/manual, hardware, release build, full workspace tests, MSRV, dependency gates и stable coverage NOT RUN по §6.3.
- Product API change ограничен новым codec-core H.264 probe; config/persistence DTO, queue domain API, player API, extractor policy и durable source shape не менялись.
- Planned local commit: `test(web-media): ratchet extractor-free lifecycle coverage`.
- Следующая session — N15; N15 не начиналась.

Related: `mem:testing/native-web-ingress-n14a-2026-09-01`, `mem:codec-core/h264`, `mem:media-services/native-hls-live-n08-2026-09-01`, `mem:app-egui/playlist-persistence-s14`, `mem:player-core/dynamic-live-timeline-s31l-2026-07-23`.