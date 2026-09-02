# G3 native web ingress final gate (2026-09-02)

## Scope и authoritative acceptance

- G3 завершён на `main` одним агентом без feature-wave. Exact `user/web-media-playlist-acceptance.xspf`: 13 rows, SHA-256 `1daa973aa0f16a3be93e588dd3c83a8432b2917a5b525a05eb278776bb9c6435`.
- Public N15: 11 available rows PASS through real startup A/V presentation; rows 04/12 честно `PROFILE_EXCLUDED`. Exact cold и restart process set `{row00,row08}`; обе причины `PageMediaResolution`, phase `CandidatePrimary`. 11 direct rows работают с `yt_dlp.enabled=false` и дают zero spawn. Public drift/fallback не маскировался.
- Performance authoritative dataset: `docs/native-web-ingress-n15-performance.json`. Cold native vs extractor median reductions: catalog 85.35%, first consumer 82.11%, wall 72.99%, combined CPU 26.64%, RSS 6.91%; p95 reductions 85.60%, 81.99%, 72.89%, 30.95%, 7.05%. Warm Ogg p95 catalog/consumer/seek-fwd/seek-back/refresh = 4.427/5.489/1.511/0.718/4.315 ms, 0 spawns. Warm HLS = 37.832/60.867/18.216/20.641/21.047 ms plus switch 20.439 ms, 0 spawns.
- Hardware evidence PASS on AMD Radeon 780M/Mesa: VP9 SDR auto, AV1 SDR/HDR P010 hardware, BT.2020 PQ → BT.709 BT.2446-C and WGPU readback. Настоящий HDR display/output mode NOT RUN; доказан HDR→SDR на SDR output.

## Final architecture/security audit

- Один `WebMediaCatalog`, один `ComponentVariantCatalog`, один `ActiveMediaSource::Web`, один `WebMediaSourceIntent`; duplicate catalogs и parallel active-source architecture отсутствуют.
- Extractor DTO остаются только в exact ratchet allowlist legacy extractor-adapter subsystem; direct/native intents, durable source, public catalog и sidebar actions нейтральны. Process creation принадлежит `service-ytdlp`.
- Один `NativeWebFallbackOwner`: один claim только до `Installed` для exact allowlist. `installed()` не хранит extractor locator, а post-installed matrix запрещает extractor для всех triggers.
- HLS/DASH/Smooth/HDS используют fetched-root handoff и exact request accounting; hidden repeated root fetch/probe не найден.
- Persistence/debug/error tests и guardrails подтверждают отсутствие temporary endpoints, raw locators, headers/cookies/credentials в durable state/logs. Typed cancellation/network/malformed/expiry/backpressure/decoder/render outcomes не схлопнуты в bool/error fallback.
- Refactor guardrails подтвердили module budgets/ownership; functional tests доходят до production consumers.

## Исправленные G3 defects и commits

1. `bcc5c9cd` — DASH ignored subtitle representations валидируются у parser owner: exact text/VTT или MP4 wvtt/stpp; DRM/unknown остаются typed terminal.
2. `95a9e77d` — adaptive exposed-prefix functional regression доказывает `ExposedContentLengthExceedsResource`.
3. `5266cad3` — cross-source queue HLS → DASH → Smooth → DASH: старый source жив до consumer success нового; каждый шаг достигает video decode, общего WGPU submit/readback и nonzero PCM до queue commit; process spy 0.
4. `6b686ab8` — disconnected playlist resume worker exit стабилизирован через production worker path.
5. `e712dd2c` — pre-cancelled preparation executor стабилизирован: task не выполняется, typed Cancelled, worker loop завершается.
6. `210cbd11` — stale failed и stale mismatched progressive seek outcomes отдельно доказывают продолжение worker-а до latest packet; устраняет LCOV derived `-1`.
7. `c509029e` — neutral `PacketDecodeStartInitialization` перенесён в `media-core`; MPEG-TS публикует evidence, HLS потребляет без зависимости на `codec-core`.
8. `5e57639f` — H.264 decode-start classifier делает один NAL pass и различает NotKeyframe / RequiresTrackConfiguration / IncludesInBandConfiguration.
9. `d61a2d87` — deterministic playback-intent wake regression проходит production `select_biased!`, exact installed update, typed receipt, session Paused и consumer-visible snapshot.

Cross-source proof покрывает production media/queue boundary, но не поднимает windowed `AppState` и не нажимает UI Next. Для точного remaining UI issue нужна конкретная row pair + symptom; gap не выдан за UI fix.

## Coverage v2 qualification

- Final qualification source: `d61a2d87`; exact intersection 9 measured workspace runs, 3 independent cohorts.
- Cohort hashes: `sha256:404996c890975fe666573751a60c86ec05c019cbf0f42be73ab7da4c3611d0d7`, `sha256:8f275aaa75182eba4d42ac12328edd62e4d6a56a9f233d60c974f38b6b07d54f`, `sha256:2b1418916b92d1ae0b595d60bb891d4ddd0d9d8aa429b9b0a8ec788869a2ac3e`.
- Logical baseline `sha256:ff51d2799a3562816de9a5f919bedb5594dc96c93b772e6e9d45c9f94b7f9743`; tracked raw SHA-256 `8c98f6acb996d9520b58703d29efb3f150bd8ba2cb60813610f1edd4936cf67b`; source-files hash `sha256:30790a092145e379c73aa0c990a2c7aa6f2a9480287f649b71698f05bd3a7383`.
- Workspace stable: functions 15,696/19,914; lines 163,462/211,068; regions 205,271/268,471.
- Atomic G2→G3 transition PASS. Единственная exact bounded exception: `crate:web-media-adaptive/regions` 2890/3239 → 2904/3255, 16 новых exposed-prefix regions, 14 stable 9/9, review_by 2026-12-01. Same-universe exact loss отсутствует.
- File-local audit: 58 changed Rust files; падения stable count живого кода нет. 18/18→17/17 в HLS discovery — удалённая iterator closure. Два свежих `scripts/coverage.sh check` PASS с пустыми regressions/universe_changes; последний cohort hash `sha256:0f63da3c8df0e721c28da36f80f74be2c894658dd4e500418e487f945b5d400b`.
- Sandbox-only loopback bind failures и incomplete cohorts не принимались и не смешивались. Один qualification HLS lifecycle failure без payload не вошёл; exact instrumented retry 100/100 и последующие 15 full-workspace runs не повторили его. Deadline/semantics не ослаблялись.

## Final gates

- Canonical `scripts/pre-pr-checks.sh` PASS. Он уже вызвал `scripts/ci-checks.sh all`; второй полный запуск не делался.
- Внутри PASS: fmt/diff policy, toolchain policy, MSRV 1.92, guardrails, smoke self-tests, strict workspace Clippy/rustdoc, full workspace tests, no-default-features, `cargo deny check`, `cargo machete --with-metadata`.
- `cargo +1.96.0 build --workspace --all-features --release --locked` PASS; final binary SHA-256 `15db8697bc15d78d13201ff3883086adf0992d1deb4dfa874ab814f88bcc452f`.
- Final Serena diagnostics: 15/16 sensitive changed files clean. `native_cross_source_playlist.rs` даёт stale LSP E0277 на уже разыменованном `*root_url`; exact file при этом успешно скомпилирован full workspace tests, strict Clippy и rustdoc. Source после 9-run baseline ради analyzer-only сигнала не менялся.
- Authoritative docs: `docs/native-web-ingress-n15-acceptance.md`, `docs/native-web-ingress-n15-acceptance.json`, `docs/native-web-ingress-n15-performance.json`.

Связанные memories: `mem:core`, `mem:testing/coverage`, `mem:testing/native-web-ingress-n14b-2026-09-02`, `mem:testing/native-web-ingress-n15-2026-09-02`. Новую feature-wave после G3 не начинать.