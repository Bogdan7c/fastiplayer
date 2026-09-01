# N12 native HDS/F4M VOD без yt-dlp (2026-09-01)

## Outcome

- Прямой HTTP(S) URL с case-insensitive `.f4m` suffix последнего path segment теперь admitted через syntactic hint, после чего authoritative F4M parser подтверждает HDS content.
- Valid supported HDS VOD открывается без yt-dlp через существующие `web-media-hds` resolve/catalog/runtime и F4F demux boundaries.
- Initial open, semantic coupled rendition switch и controlled reopen выполняют fresh stable-root fetch/rematch; durable state хранит stable root, source lineage и neutral semantic selection, но не bootstrap/fragment URL.
- Receipted HDS seek сохранён на existing worker-owned `PreparedDemuxSeekPort`. Initial selected-fragment probe передаёт уже открытый demuxer runtime-у и не повторяет selected `Frag1`.

## Ownership and boundaries

- `app-egui/url_service_adapter/native_hds.rs` владеет только URL hint и admission wiring. Query/fragment не участвуют в suffix classification; FTP(S) не admitted.
- `app-egui/startup_media/native_hds.rs` владеет одной bounded root GET попыткой, source generation, stable snapshot identities, app capability probe, typed fallback allowlist, startup job/cancellation/shutdown и endpoint recovery.
- `app-egui/media_open/native_hds.rs` владеет stable root/lineage, neutral selection/catalog attachment и semantic switch/reopen intents.
- `app-egui/media_open/native_hds_preparation.rs` изолирует HDS-specific preparation/install от большого neutral dispatcher.
- `app-egui/web_media_open/hds.rs` композирует fetched-root discovery, neutral coupled catalog, semantic rematch, exact row open, playback window и receipted seek attachment.
- `web-media-hds::HdsFetchedManifestInput` и `discover_fetched_hds_renditions` являются новым fetched-root boundary: target, source generation, VOD presentation и current byte budget проверяются до resolver-а; общий complete-pass сохраняет существующие capability probes и eager demux handoff.
- `web-media-hds::resolve_fetched_presentation` меняет только источник bytes root document; child manifests, external bootstrap и F4F fragments остаются у existing resolver/transport.
- `hds-manifest-core` теперь различает `InvalidRoot`, `DrmProtected`, `PrivateExtension`, unsupported profile и malformed/schema errors. F4F/FLV parsing, packet ownership и guardrails не переносились из их владельцев.

## Error and fallback invariants

- Initial extractor fallback разрешён только для well-formed foreign XML root (`InvalidRoot`) и HTTP 401/403 authorization response.
- Malformed F4M, unsupported profile, DRM, private extension, live manifest/bootstrap, general network error и cancellation являются distinct typed terminal categories и не маскируются fallback-ом.
- Exact switch/reopen никогда не сохраняет extractor locator и не может молча сменить semantic stream.
- Raw query/secret material не попадает в Debug/safe label/catalog identity/diagnostics.

## Functional evidence

Hermetic app vertical `native_hds_switch_seek_reopen_reaches_h264_aac_without_extractor`:

1. выполняет direct root fetch с query, проверяет exact one root GET per attempt;
2. capability-probe-ит две coupled rows и проверяет exact one `Frag1` per row/attempt;
3. получает `TracksChanged`, принимает и получает receipt nonzero VOD seek;
4. доводит реальный H.264 Annex-B fixture через F4F/FLV demux и FFmpeg decoder до offscreen WGPU submit/readback/release;
5. доводит AAC-LC fixture до nonzero PCM;
6. semantic-switch-ит coupled row после перестановки manifest rows;
7. controlled-reopen-ит ещё один fresh snapshot и сохраняет source lineage;
8. подтверждает `YtDlpExtractorAdapter` process spy = 0.

Vertical прошёл обычным cohort-ом 3/3. Отдельный `web-media-hds` regression доказывает root GET = 1 и selected eager `Frag1` = 1 при fetched-root handoff.

## Verification

PASS:

- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo test -p hds-manifest-core -p web-media-hds --all-targets --all-features --locked --no-fail-fast` — 30 tests
- `cargo test -p app-egui --all-features --locked native_hds -- --nocapture` — 4 tests
- three ordinary runs of `native_hds_switch_seek_reopen_reaches_h264_aac_without_extractor` — 3/3
- `cargo test -p app-egui --all-features --locked same_item -- --nocapture` — 7 tests
- `cargo test -p app-egui --all-features --locked controlled_reopen -- --nocapture` — 1 test
- `cargo clippy -p hds-manifest-core -p web-media-hds -p app-egui --all-targets --all-features --locked -- -D warnings`
- `cargo check --workspace --all-targets --all-features --locked`
- Serena warning/error diagnostics for all changed production boundaries — clean.

One initial vertical invocation inside the filesystem sandbox could not bind its hermetic localhost listener (`EPERM`); the approved loopback execution then passed all three ordinary runs. This was an execution sandbox restriction, not a product/test failure.

## Explicitly not run / scope

- Public-network media, GUI/manual acceptance and hardware decode were NOT RUN and are not claimed.
- HDS live/DVR and DRM remain intentionally excluded.
- No new parser, HDS runtime, F4F demux or decoder implementation was introduced; N12 composes existing owners.
- N13A was not started.

Local commit message: `feat(hds): open direct manifests without yt-dlp`.