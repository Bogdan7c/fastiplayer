# N14A hermetic protocol consumer matrix (2026-09-01)

## Outcome

- Добавлен focused filter `n14a_consumer`: 10 hermetic functional tests покрывают 12 обязательных rows — HTTP Ogg, HTTP WebM, FTP Ogg; HLS VOD TS/fMP4 и sliding live; DASH VOD fMP4/WebM и dynamic live; Smooth VOD; HDS VOD; extractor-backed page.
- Ни один PASS не останавливается на classification/manifest/demux. Video rows проходят production demux -> FFmpeg decode -> HostPlanar WGPU materialization -> renderer submit -> nonzero readback -> GPU-completion-aware release. Audio rows дают nonempty production PCM и проводят exact interleaved sample count через production `AudioClock::record_written/record_played`, после чего clock position строго больше initial.
- Новый playback/test runtime не создавался: переиспользованы existing `RangeFixtureOrigin`, `FtpVorbisOrigin`, `ControlledHlsServer`, protocol fixture builders, production decoders и `OffscreenWgpuHarness`.

## Ownership and boundaries

- `web_media_open/content_probe_tests.rs` остаётся HTTP origin/composition owner-ом и теперь считает successful response-body bytes отдельно от request count.
- `content_probe_tests/ftp_vorbis.rs` остаётся FTP control/data owner-ом и считает exact RETR count плюс data-body bytes.
- `media_open/web/tests/native_hls_vertical.rs::ControlledHlsServer` остаётся общим adaptive loopback origin-ом и хранит per-request path + response-body byte count.
- N14A-specific page launcher и PCM/clock assertion вынесены в `web_media_open/content_probe_tests/n14a_consumer.rs`, чтобы не раздувать parent test owner. Launcher injected в тот же production `YtDlpExtractorAdapter`, который обслуживает attempt; global hook/default launcher не используется.
- Production boundaries, public/internal product API, config, persistence и dependency graph не менялись.

## Exact accounting

- HTTP Ogg initial: classifier 0 requests; open/PCM 3 Range requests и 73 077 media bytes; `yt_dlp.enabled=false`.
- HTTP WebM initial: classifier 0; open/decode/render 2 Range requests и 1 578 media bytes; `yt_dlp.enabled=false`.
- FTP Ogg initial: classifier 0 RETR; open/PCM 6 RETR и 219 634 media bytes; `yt_dlp.enabled=false`.
- Extractor page: before open 0 process/0 HTTP; exact one `PageMediaResolution` process attempt; candidate HTTP 2 requests и `fixture_len + 1` bytes (one-byte capability probe + exact Ogg body).
- HLS VOD TS/fMP4: root 0 before open, 2 root GET after two row opens, body bytes equal exact first+second master snapshots, injected process spy 0.
- HLS sliding live: root 0 before open, exact one initial master GET/body before consumer movement, process spy 0.
- DASH VOD fMP4/WebM: root 0 before open, 2 root GET and exact two MPD snapshot byte sum, process spy 0.
- DASH dynamic live: root 0 before open, exact one initial MPD GET/body before consumer movement, process spy 0.
- Smooth VOD and HDS VOD: root 0 before open, exact one root GET/body, process spy 0; HDS также сохраняет existing exact eager Frag1 probe accounting.
- Direct paths structurally не имеют extractor adapter boundary и работают при disabled extractor; adaptive owners используют fail-fast injected spy и требуют exact 0; page row требует exact 1.

## Verification

PASS:

- ordinary three-run cohort `cargo test -p app-egui --all-features --locked n14a_consumer -- --nocapture` — 10/10, 3/3 runs;
- `cargo test -p app-egui --all-features --locked media_open::web::tests::native_ -- --nocapture` — 16/16;
- `cargo test -p app-egui --all-features --locked web_media_open::content_probe_tests -- --nocapture` — 13/13;
- `cargo test -p app-egui --all-features --locked web_media_extractor_adapter::tests -- --nocapture` — 5/5, включая provider DTO/source-shape ratchets;
- `cargo clippy -p app-egui --all-targets --all-features --locked -- -D warnings`;
- `cargo check --workspace --all-targets --all-features --locked`;
- `cargo fmt --all -- --check`, `git diff --check`;
- Serena diagnostics clean for all changed/new modules; parent сохраняет прежний rust-analyzer false positive для external `audio_fixtures` path, Cargo/Clippy проходят.

## Scope and handoff

- Product bug не обнаружен; изменения test-only.
- Queue navigation, restart и full cross-protocol switch matrix не добавлялись. HLS/DASH VOD используют только один existing semantic row-selection boundary, необходимый для consumer proof второй mandatory row; seek/reopen/queue orchestration остаётся N14B.
- Public-network, GUI/manual, hardware, release build, full workspace tests, MSRV, dependency and stable coverage gates NOT RUN по §6.3.
- Planned local commit: `test(web-media): prove native protocols reach media consumers`.
- Следующая session — N14B; в N14A не начиналась.
