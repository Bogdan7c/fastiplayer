# G2 native-ingress qualification, coverage и push (2026-09-01)

## Scope и итог

- Gate-only session для accumulated N06–N13B; новая feature logic не добавлялась. Self-review проверил duplicate root fetch, provider DTO isolation, fake-catalog combinations, transient secret/endpoint persistence, post-Installed fallback и error taxonomy.
- Duplicate fetch закрыт fetched-body type-state и exact root accounting. DASH/Smooth/HDS catalogs строятся из proven lanes/compatibility edges, не Cartesian product. Provider DTO source ratchet и persistence unknown-field/material scans зелёные. После Installed все fallback triggers terminal; cancellation/network/malformed/DRM/runtime distinctions не collapsed.
- Public network/GUI/hardware playback не запускались: G2 использует hermetic loopback/offscreen WGPU/FFmpeg software verticals. Следующая разрешённая работа — N14A; в этой session она не начиналась.

## Gate-only root fixes

- `ba3b0b68` восстановил S42 module/dependency guardrails без feature behavior: большие app/DASH symbols вынесены в `media_open/preparation/tests.rs`, `media_open/web/source_actions.rs`, `startup_media/orchestration/drain.rs`, `web-media-dash/catalog/default_selection.rs`; stale `service-direct-media -> media-prefetch/web-media-http` required edges заменены exact forbidden normal-edge ratchet. Production files снова ниже hard 800-line limit.
- `201ab746` синхронизировал frozen playlist-discovery cancellation fixture через existing blocking probe gate; production semantics не менялись.
- `5259ef71` закрепил public Smooth `InvalidRoot`/`MalformedSchema` Debug+Display diagnostics и устранил coverage drop без exception.
- `169ade5c` добавил deterministic real-thread disconnect regression для resume worker: close wake endpoint, bounded exit observation, join.
- `3f9d5f90` устранил dynamic-options fixture races: второй provider-call становится explicit happens-before для active+retired shutdown, idle shutdown отдельно закрепляет Completed/AlreadyCompleted fallthrough. Production semantics не менялись.

## Hermetic vertical/process matrix

- Native extractor set exact empty `{}` и injected production-boundary spy count 0 для direct HTTP Ogg/WebM, FTP Ogg, HLS VOD TS+fMP4, HLS live, DASH static H.264/AAC+VP9/Opus, DASH dynamic live, Smooth VOD и HDS VOD; те же paths проходят при `yt_dlp.enabled=false`.
- Native verticals достигают реального decoder/render/audio результата: FFmpeg/VP9 decode, offscreen WGPU submit/release и nonzero PCM. Seek/switch/endpoint recovery/semantic reopen имеют exact request accounting, root count 0 до open и не выполняют duplicate classifier fetch.
- Process-positive hermetic set: YouTube-like page — exactly `PageMediaResolution/CandidatePrimary`; HTML recovery — exactly `PageMediaResolution/{CandidatePrimary,RecoveryPageCapture,RecoveryEmbedCandidate}`; collection topology — exactly `CollectionTopologyResolution/TopologyPrimary`; cancellation recovery — exactly два `ExtractorBackedRecovery` launches и descendant process-group reap.
- Production process allowlist exact: direct `.spawn()` только `crates/service-ytdlp/src/invocation.rs`; `spawn_owned_process_with_launcher` только callers `process.rs`, `topology/process.rs` и owner definition `process_tree.rs`.

## Verification

PASS:
- `scripts/pre-pr-checks.sh` (exact wrapper над `scripts/ci-checks.sh all`): metadata/toolchain, dependencies, 230 guardrails, scripts, S42/refactor, fmt, cargo-deny/machete, standalone patches, strict workspace Clippy/rustdoc, workspace tests, app no-default-features, MSRV 1.92.
- `cargo build --workspace --all-features --release --locked`.
- Native app verticals 10/10; direct progressive verticals 4/4; service-ytdlp invocation 6/6; extractor adapter 5/5; fallback matrix 3/3; playlist-state full suite and focused regression suites.
- Security/persistence/provider DTO/process source ratchets PASS. Release GUI/public/hardware acceptance NOT RUN by design.

## Stable coverage

- Authoritative details: `mem:testing/coverage`.
- Финальный baseline source revision `3f9d5f90`; three independent cohort hashes: `sha256:18b816b0067f5f4eda21600cbd2b8852d95e2f484a102eac8aadb2075d7ab875`, `sha256:be8657278eb85c32148f703f61045616ee87b2eae914b64468abf97308d00e6e`, `sha256:ae578adf3c0c3aa9e0365d31f8ebb1eee59a6975b9f75489290fbe26016cc7b1`.
- Exact 9-run intersection: workspace functions 15,652/19,879, lines 162,784/210,449, regions 204,329/267,594; blocking functions 9,846/11,681, lines 99,471/115,753, regions 124,744/148,339.
- Baseline logical hash `sha256:4295f9a05fb06ba6a11d04d5623d154c371c758224f6a88e84f7938067267afb`, raw SHA-256 `3c04e5d97e7d806dc05f481b4f536ebbc7935861d898ae7f06ffe1ab88d5050a`. Empty ledger raw SHA-256 remains `1f64ad40d0db9ebf1a108da65cd02c8baec6a26c41e78e85add972c6f3534a2b`.
- Atomic update PASS, file-local decreases 0 relative to installed predecessor, two fresh repeatability checks PASS with `universe_changes=[]`, `regressions=[]`, exceptions 0 and check hash `sha256:92e76ae086a9b6ae8a2820411032d0231b93d00e4a5814691341613d5eeed6be`.

Related: `mem:core`, `mem:testing/coverage`, `mem:media-services/native-web-ingress-n13a-2026-09-01`, `mem:media-services/native-web-ingress-n13b-2026-09-01`, `mem:playlist/discovery`.
