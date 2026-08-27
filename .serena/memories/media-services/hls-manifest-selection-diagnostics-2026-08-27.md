# HLS committed manifest-selection diagnostics (2026-08-27, neutral log boundary)

## Confirmed HLS-owned data/lifecycle

`HlsManifestSegmentSeekMarker` is built only from packet-derived `HlsSeekAnchor` provenance and contains safe scalar fields:

- phase: `initial_open | initial_restore | preview | final_receipt`;
- component role: `muxed | video | audio`;
- HLS-local opaque nonzero unique `manifest_selection_id`, allocated by a process-global counter at staging but potentially emitted out of numeric order after commit authority (not a public operation/request correlation id);
- landing policy, source generation, requested target;
- truthful `actual_anchor_ms`, decode anchor and anchor kind;
- media/discontinuity sequence, global manifest index, epoch/restart index;
- half-open manifest interval `[segment_start_ms, segment_end_ms)`.

No URI/path/query/header/cookie/token/key/map locator/hash/cache key/resource id/request id is present.

`HlsManifestSeekPoint` flows through planned media resource → plaintext byte span → packet byte offset → anchor. Init resources never pretend to own a media segment.

## Commit ordering

- InitialOpen/InitialRestore markers are staged from exact packet evidence.
- Single-component cold marker survives only after topology validation, active-read activation, final demuxer assembly and successful initial-position proof publication.
- Separate A/V cold markers are committed only after both topology validations, both active-read activations, successful final composite assembly and successful shared initial-position proof publication. The staged pair is dropped together on any failure.
- Offside receipted candidate no longer mutates shared `HlsSeekIndex` during prepare. The proven anchor and marker are staged together.
- Cancellable seek calls `complete()` before active-read activation/composite assembly. Only then, immediately before component/A-V swap, the staged shared anchor and marker become commit evidence.
- Dropped/cancelled/failed replacements do not mutate the shared preview index. Focused tests cover drop vs authorized commit.
- Separate audio retains the public requested target rather than its internal video-alignment target.

## Production emission boundary: neutral `log` facade

Пользователь выбрал вариант A: `web-media-hls` имеет узкую normal dependency на workspace `log = 0.4.29`. Guardrail разрешает только neutral facade с явным инвариантом: concrete logger/backend принадлежит composition root, HLS не зависит от `tracing`, subscriber-а или app.

`HlsManifestSegmentSeekMarker::emit()` публикует INFO record с target `rustiplayer::hls_manifest_selection`; message — только точный secret-safe `Display` marker. App `tracing-subscriber` собирается с default `tracing-log` bridge, поэтому существующий default INFO runtime filter принимает facade record без дополнительного HLS/app API. Unit capture logger ставится один раз без panic внутри `OnceLock`, сериализует capture и принимает только current test thread/точный target, поэтому параллельные unit tests не загрязняют доказательство.

Commit ordering не изменён: `emit()` остаётся вызываемым только из authorized staged commit. Cancelled, failed, superseded и stale candidates marker не публикуют. Реальные acceptance-метрики не заявлены до отдельного запуска.

## Offline acceptance consumer (2026-08-28)

- `scripts/playback_acceptance_hls.py` parses the exact `Display` line into independent selection records and deliberately has no public seek/scrub correlation input.
- All numeric formatter fields are strict decimal u64. Known enums, nonzero opaque selection-ID uniqueness within one log source, half-open interval and provable anchor/role consistency are fail-closed typed anomalies surfaced through JSON, table and `--strict`. Numeric ID order is not a schema invariant because staging allocation and commit-time emission are separate lifecycle points; same-role `2 -> 1` is valid.
- Cold (`initial_open|initial_restore`) and warm (`preview|final_receipt`) rows retain requested target, exact selected segment and actual/decode anchors separately by component role. Legacy/non-HLS reports add empty HLS sections without inventing missing-marker anomalies.
- Production INFO marker теперь доступен offline consumer-у через target `rustiplayer::hls_manifest_selection`; parser по-прежнему не приписывает ему public-operation correlation. Реальные raw-log acceptance результаты должны подтверждаться отдельным запуском.

## Confirmed tests/checks

- Full 3-crate all-target/all-feature tests are green, включая production-level INFO capture точного target/message и privacy deny-list.
- HLS cold restore/open runtime suite, discontinuity landing, separate A/V atomic seek and failure paths are green.
- Shared-index staged drop/commit unit tests are green.
- Strict 3-crate clippy, workspace fmt, diff check, refactor/S42/format guardrails are green.
- Focused offline HLS analyzer/CLI suite is green (12 tests); no public-operation correlation was added.
- Exact grouped-TS flaky test passed 10/10 repetitions.

Related: `mem:media-services/hls-preview-receipt-cancellation-2026-08-27`, `mem:media-services/hls-vod-manifest-receipted-seek-2026-08-24`.
