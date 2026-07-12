# Rustiplayer readiness report — 2026-07-12

## Verdict

**Итог: `NOT READY` для безусловного старта крупного feature roadmap.**

Correctness и основные архитектурные boundaries после Сессий 01–27E в основном доказаны. Однако общий Definition of Done требует одновременно зелёных foundation gates, а на snapshot `a9d3c86` остаются два воспроизводимых blocking failure:

1. `scripts/ci-checks.sh dependencies` блокируется `RUSTSEC-2026-0194` и `RUSTSEC-2026-0195` в `quick-xml 0.39.3` через `wayland-scanner 0.31.10`.
2. `scripts/coverage.sh check` блокируется coverage ratchet после переноса inline tests в отдельные `tests.rs`/`tests/` во время decomposition sessions.

Это не субъективный запрет «код ещё недостаточно красивый». Все остальные обязательные local gates ниже воспроизводимо прошли. До закрытия двух failures разрешены bounded remediation, повторный audit и manual hardware validation; крупную новую функцию начинать нельзя.

## Проверенный snapshot и методика

- Ветка: `main`.
- Commit до начала аудита: `a9d3c86` (`перенесены доки по разбиению`).
- Исходное рабочее дерево: clean, `main...origin/main`.
- Primary toolchain: `rustc 1.96.0`, LLVM `22.1.2`.
- Проверяемый MSRV: Rust `1.92.0`.
- Context7: сверены официальные Cargo semantics `rust-version`, `--locked`, feature selection и default file exclusions `cargo-llvm-cov 0.8.7`.
- Для честного line census исходный snapshot плана `6603376` был извлечён во временный `/tmp` и проверен той же командой, что current tree. Получено точное совпадение с baseline 81/47.
- Coverage baseline и `coverage/exceptions.json` не изменялись; exceptions остаются пустыми.

`--locked` рассматривается именно как запрет изменения/отсутствия `Cargo.lock`, а не как offline mode. Feature-off и all-features конфигурации проверялись отдельными командами.

## Результаты gates

| Gate | Команда | Результат |
| --- | --- | --- |
| Metadata/toolchain/fixtures/guardrails/fmt | `scripts/ci-checks.sh format-guardrails` | **PASS**: locked metadata, MSRV 1.92/primary 1.96 policy, 4 patch inventory entries, 23 Python tests, shell syntax, smoke self-tests/schema v5, refactor guardrails, rustfmt |
| Locked primary check | `cargo +1.96.0 check --workspace --locked` | **PASS** |
| Strict Clippy | `scripts/ci-checks.sh clippy` | **PASS**, all targets/all features, warnings denied |
| Strict rustdoc | `scripts/ci-checks.sh docs` | **PASS**, all features, warnings denied |
| Hermetic all-features tests | `scripts/ci-checks.sh tests` | **PASS**, workspace, locked, no-fail-fast; manual media/runtime tests остались explicit `ignored` |
| App feature-off | `scripts/ci-checks.sh app-no-default-features` | **PASS** |
| Real MSRV | `scripts/ci-checks.sh msrv` | **PASS** на Rust 1.92.0 |
| Patch integration | `scripts/ci-checks.sh dependency-patches` | **PASS**: inventory + `audio`, `symphonia-demux`, `video-vaapi` integration tests |
| `cros-libva` standalone | `cargo test --manifest-path crates/cros-libva-patch/Cargo.toml --locked` | **PASS** compile/doc; 2 hardware demos intentionally ignored |
| `cros-codecs` standalone | `cargo test --manifest-path crates/cros-codecs-patch/Cargo.toml --locked` | **PASS**, 58 tests |
| `symphonia-format-isomp4` standalone | `cargo test --manifest-path crates/symphonia-format-isomp4-patch/Cargo.toml --locked` | **PASS**, 20 tests |
| `symphonia-codec-aac` standalone | `cargo test --manifest-path crates/symphonia-codec-aac-patch/Cargo.toml --locked` | **PASS**, 5 tests |
| Dependency/security/license policy | `scripts/ci-checks.sh dependencies` | **FAIL**: два blocking advisories; licenses/sources/bans прошли, unused direct dependencies не найдены |
| Coverage | `scripts/coverage.sh check` | **FAIL**: clean suite прошла, report создан, ratchet обнаружил decreases |
| Smoke dry-run contract | `scripts/runtime-acceptance.sh --suite playback-matrix ... --dry-run` | **NOT RUN**, exit 0, явно `acceptance not satisfied` |
| FFmpeg runtime probe | `scripts/playback-smoke.sh --mode probe-only` | **PASS**: 8 focused probe tests + реальный ignored runtime probe |
| Force-warn dead code | `cargo clippy --workspace --lib --all-features --locked -- --force-warn dead-code` | **PASS**, findings отсутствуют |
| Panic census | `cargo clippy --workspace --lib --locked -- -W clippy::unwrap_used -W clippy::expect_used` | **PASS как census**: 38 `expect`, 0 `unwrap`; findings не скрыты |

### Guardrail test quality

`python3 -m unittest discover -s scripts/tests -v` выполнил 23 positive/negative tests. Они проверяют разрешённое и запрещённое направления dependency graph, изоляцию FFmpeg, запрет возврата `video-vulkan`, public backend options, VA display ownership, второй `PlayerSession`, CPU RGB conversion, tempo boundaries, точный MSRV/metadata inheritance и coverage exception/ratio semantics.

## Metadata и CI truth

`cargo metadata --locked --no-deps --format-version 1` подтвердил:

- 31 workspace package;
- effective `rust-version = 1.92` у всех package;
- effective `license = MIT` у всех first-party package;
- package без `rust-version` или license отсутствуют.

`.github/workflows/ci.yml` содержит отдельные locked jobs: coverage ratchet, четыре standalone patch entries, patch integration, dependency policy, format/guardrails, strict Clippy, strict docs, all-features tests, app no-default и MSRV 1.92.

GitHub branch protection остаётся owner-approved operational limitation. Повторные read-only запросы rulesets и `main` protection вернули HTTP 403: private repository требует GitHub Pro или public visibility. Поэтому workflow failures видимы, но merge protection обеспечивается вручную.

## Fixture hermeticity

В first-party source нет обращения к `test-assets/`. Serena reference audit нашёл `RUSTIPLAYER_MEDIA_PATH` только в:

- одном `service-direct-media` manual test;
- трёх `service-youtube` manual transport tests;
- `symphonia-demux/tests/support/manual_media.rs`.

Все consumers этих helpers имеют `#[ignore = "manual media regression; use scripts/media-regression.sh"]`. Обычный all-features workspace suite прошёл без выбора local media. `scripts/media-regression.sh` и runtime acceptance требуют один явный путь и не ищут owner-local assets.

## Coverage evidence и причина failure

Clean tests и создание JSON/LCOV/HTML завершились успешно. Артефакты находятся в ignored `target/coverage/`. Ratchet правильно отказался принимать снижение без exception.

| Scope/metric | Versioned baseline | Current | Status |
| --- | ---: | ---: | --- |
| Workspace lines | 58,981 / 81,342 | 57,050 / 80,520 | decrease |
| Workspace functions | 5,815 / 7,775 | 5,713 / 7,733 | decrease |
| Workspace regions | 73,032 / 101,181 | 69,990 / 99,211 | decrease |
| Blocking group lines | 36,977 / 43,992 | 34,353 / 41,510 | decrease |
| Blocking group functions | 3,732 / 4,356 | 3,539 / 4,163 | decrease |
| Blocking group regions | 45,651 / 54,358 | 41,937 / 50,802 | decrease |

Наиболее заметные line changes:

| Crate | Baseline | Current |
| --- | ---: | ---: |
| `codec-core` | 4,162 / 5,240 | 3,390 / 4,462 |
| `config` | 2,390 / 2,624 | 1,195 / 1,403 |
| `player-core` | 12,553 / 15,157 | 12,784 / 15,518 |
| `render-core` | 1,476 / 1,635 | 775 / 932 |
| `rustiplayer-settings` | 890 / 1,026 | 1,180 / 1,411 |
| `service-direct-media` | 370 / 489 | 370 / 493 |
| `video-core` | 1,491 / 1,697 | 849 / 1,033 |

Причина доказана по current LLVM file inventory и документации `cargo-llvm-cov`: default regex исключает каталоги `tests/`, файлы `tests.rs` и `*_tests.rs`. Исходный baseline учитывал тестовые функции, пока они находились inline внутри production-файлов. Decomposition sessions перенесли те же тесты в отдельные test modules; LLVM перестал включать их в report. Это объясняет резкое изменение `config`, `render-core`, `video-core` и `codec-core` без исчезновения самих тестов.

Нельзя просто записать current counters как новый baseline: сначала требуется отдельное решение о стабильной классификации test code и точные versioned exceptions для осознанной миграции. См. [coverage follow-up](user/session_28_followup_coverage_baseline_after_decomposition_2026-07-12.md).

## Повторный census относительно baseline

### Сопоставимая методика

Scope для line census: tracked `*.rs`, `*.py`, `*.sh` внутри `crates/` и `scripts/`, без standalone `*-patch`, каталогов `tests/` и отдельных `tests.rs`/`*_tests.rs`. Inline tests остаются частью физического production-файла — это ровно воспроизводит исходные 81/47 на commit `6603376`.

| Метрика | Baseline | Current | Изменение |
| --- | ---: | ---: | ---: |
| Файлы ≥700 физических строк | 81 | 76 | −5 |
| Файлы ≥1000 физических строк | 47 | 40 | −7 |
| First-party `dead_code` allow sites | 4 | 0 | −4 |
| Production `expect` findings | 57 по исходному плану | 38 | −19 |
| Production `unwrap` findings | 2 | 0 | −2 |
| Unsafe sites, одинаковая regex, весь first-party Rust | 147 | 141 | −6 |
| Unsafe sites, production-only scope | 140 | 134 | −6 |

Примечание: canonical panic policy зафиксировал свой pre-fix Session 11 census как 51 `expect` + 2 `unwrap`; исходный общий план содержит 57 + 2. В verdict сохранён baseline общего плана, а current результат получен повторной командой Clippy.

`tokei 14.0.0` использован только как дополнительный срез current production scope: 350 файлов, 118,339 code lines, 2,045 comment lines и 18,817 blank lines. Он не использовался для улучшения baseline-цифр.

Крупнейшие current production/script файлы:

| Файл | Физические строки |
| --- | ---: |
| `rustiplayer-settings/src/routing.rs` | 1,860 |
| `player-core/src/diagnostics.rs` | 1,848 |
| `capability-core/src/selection.rs` | 1,646 |
| `video-vaapi/src/codec_adapter.rs` | 1,580 |
| `render-wgpu-video/src/video/mod.rs` | 1,573 |
| `app-egui/src/frame_prepare.rs` | 1,569 |
| `codec-core/src/adapter.rs` | 1,543 |
| `render-wgpu-video/src/video/host_planar_upload.rs` | 1,497 |
| `player-core/src/seek_state.rs` | 1,480 |
| `codec-core/src/h265.rs` | 1,465 |
| `service-youtube/src/resolver.rs` | 1,451 |
| `symphonia-demux/src/track_mapper.rs` | 1,413 |
| `service-youtube/src/lib.rs` | 1,400 |
| `render-wgpu-video/src/video/p010_renderer.rs` | 1,396 |
| `settings-core/src/controller.rs` | 1,384 |

Decomposition дала измеримый результат, но не обнулила size debt. Примеры: `video-vaapi/decoder.rs` 3,658 → 1,270; `render-core/lib.rs` 3,267 → 36; `config/schema.rs` 3,052 → 125; `video-vaapi/codec_adapter.rs` 2,760 → 1,580; `codec-core/h265.rs` 2,649 → 1,465; `video-core/decoder_thread.rs` 1,727 → 123; `app-egui/ui/timeline.rs` 1,585 → 21. `player-core/diagnostics.rs` остался 1,848, потому что его owner split не входил в выполненные bounded packages.

### Dead code и panic sites

- Module-wide `#![allow(dead_code)]` и узкие first-party `#[allow(dead_code)]` отсутствуют.
- `--force-warn dead-code` чист.
- Текущие 38 `expect` не подавлены и перечислены в `docs/panic-invariant-policy.md` по owner crate. Критические thread-spawn и poisoned resource-pool boundaries уже typed/fail-closed.
- Остаточный долг включает runtime overflow/config mapping paths в `media-prefetch`, `player-core`, `frame-server-core` и `service-youtube`; их нельзя заменять механически.

### Unsafe

134 production sites сосредоточены в явных owner-модулях: FFmpeg FFI, WGPU DMA-BUF import, VA-API/GBM и один staging-copy helper. First-party `unsafe impl Send/Sync` остаются только там, где есть локальное safety proof и compile-time tests: `OwnedAvFrame: Send`, immutable `AvFrameHostPlanarOwner: Sync`, `StagingCopyBand: Send`. Недоказанные Linear GBM `Send/Sync` удалены. Unsafe не считается устранённым риском: hardware/driver behavior остаётся manual acceptance surface.

## Public/API boundary review

### Cargo graph

- `player-core` зависит от neutral contracts (`audio-core`, `capability-core`, `codec-core`, `frame-server-core`, `media-core`, `video-backend-api`, `video-core`, frame/present contracts) и не зависит от concrete `video-vaapi`, `video-ffmpeg`, render implementation или demux opener.
- `frame-server-core` имеет normal workspace edges только в `media-core` и `video-present-core`.
- `settings-core` не имеет normal dependencies и остаётся project/GPU/UI-neutral.
- `video-backend-api` зависит только от `video-core` среди workspace crates.
- `render-wgpu-video` зависит от render/video contracts и не зависит от `player-core` или app composition.
- Refactor guardrails и их negative tests подтверждают запрещённые reverse/concrete edges.

### Serena reference graph

- `ScrubCommitPolicy` идёт от app timeline intent через `PlayerCommand` к `PlayerSession::end_scrub`, где обе ветви матчятся отдельно. Focused tests доказывают разные exact targets, stale generation и typed fallback.
- Реальный settings lifecycle принадлежит `SettingsController::apply`: validate → preflight → runtime commit → persist, с typed `ApplyReport`/`ApplyFinalState`; production caller — `app-egui/settings_runtime/transaction.rs`. Tests покрывают busy/conflict, reverse rollback, persistence failure и no-op.
- `PrefetchingByteSource::new` возвращает `PrefetchStartupError`; direct-media сохраняет typed variant, YouTube добавляет context и протягивает error. Thread spawn больше не паникует.
- `DmaBufImageLayout::ComposedMultiObject` имеет typed capability rejection до decode start и focused negative test; backend guard остаётся defensive.
- `YoutubeHdrSelection` проходит из config/settings через app startup в `service-youtube::selection`; HDR выбирается только после полного capability intersection, unknown dynamic range остаётся typed rejection.
- Старые production producers `policy: _`, `DeferredTechnicalDebt`, `deferred_boundary_settings` и module-wide dead-code allow отсутствуют. Schema v4 встречается только в legacy migration tests.

## Self-review исходных P0/P1

| Исходная проблема | Verdict |
| --- | --- |
| P0-1 MSRV/toolchain mismatch | **Closed**: 1.92 truth + 1.96 pin, automatic policy и real MSRV pass |
| P0-2 CI отсутствует | **Closed с operational limitation**: locked matrix существует; branch protection недоступна private repo без Pro |
| P0-3 pre-PR допускает warnings/не запускает tests | **Closed**: strict scripts и representative tests прошли |
| P0-4 fixtures зависят от 56 GiB assets | **Closed** для CI: default suite hermetic, real media explicit/ignored |
| P0-5 dependency/security/license control отсутствует | **Infrastructure closed, gate open**: policy работает и блокирует два реальных advisories |
| P1-6 `ScrubCommitPolicy` игнорируется | **Closed**, обе semantics исполняются и тестируются |
| P1-7 settings `DeferredTechnicalDebt` | **Closed**, real transactional lifecycle и rollback semantics |
| P1-8 fallible startup/lock panics | **Critical boundaries closed**; 38 классифицированных expects остаются bounded debt |
| P1-9 dormant code скрыт allow | **Closed**, zero first-party allows и force-warn clean |
| P1-10 late/temporary limitations | **Closed как typed policy** для DMA-BUF/HDR и schema v5 smoke; full hardware acceptance не выполнена |
| P1-11 unsafe surface не проверена | **Audit closed, runtime risk remains**: owners/tests/safety proofs есть, manual hardware matrix обязательна |
| P1-12 local patches проверяются косвенно | **Closed**, четыре direct locked suites + integration suite прошли |

Отдельно foundation coverage requirement **не закрыт**, потому что ratchet сейчас красный и baseline policy нестабилен относительно перемещения test code.

## Остаточные known limitations и допустимый риск

### Blocking до feature roadmap

1. `quick-xml 0.39.3` advisories через Wayland proc-macro graph. По Cargo graph это compile/proc-macro path, а не direct runtime media parser, но policy намеренно не разрешает игнорировать vulnerability.
2. Coverage ratchet baseline несовместим с завершённым переносом test modules. Tests не потеряны, однако zero-unrecorded-regression policy формально и фактически не выполнена.

### Допустимый только как явно записанный residual risk

- `audiopus_sys 0.2.2` помечен `RUSTSEC-2026-0150` как unmaintained; безопасного upgrade advisory не предлагает. Это visibility warning, а не текущий blocking vulnerability.
- 38 production `expect` остаются по crate-local реестру; новые fallible boundaries не должны копировать этот долг.
- 141 first-party unsafe site, из них 134 в production scope; автоматические tests не заменяют реальный Intel/AMD/driver validation.
- 76 production/script файлов всё ещё ≥700 строк, 40 ≥1000. Размер сам по себе не является разрешением на косметический split.
- Native HDR output и CPU readback fallback не реализованы; active HDR path — typed HDR-to-SDR.
- Multi-object DMA-BUF остаётся явно unsupported и должен отклоняться до backend start.
- Четыре local patches остаются critical forks с обязательной direct/integration/manual media validation.
- Full playback/hardware/media suite в этой сессии **NOT RUN**, потому что владелец не передал явные fixture paths.
- Direct `playback-smoke.sh --dry-run` печатает промежуточный `PASS: FFmpeg runtime probe acceptance`, хотя команды только планируются. Canonical `runtime-acceptance.sh` исправляет итог на честный `NOT RUN`, но внутреннюю маркировку следует сделать однозначной.
- Required checks не enforced GitHub branch protection из-за private repository plan limitation.

## Manual hardware/media matrix

| Scenario | Что должно быть доказано | Статус Session 28 |
| --- | --- | --- |
| FFmpeg installed runtime probe | `libavcodec >= 62`, `libavutil >= 60`, FFI loader/version ownership | **PASS** |
| Software H.264 + VP9 stress | `ffmpeg-host-upload-wgpu`, отсутствие starvation/resource exhaustion/fatal markers | **NOT RUN**: нет explicit media paths |
| Auto AV1 fallback | exactly-once reselection в FFmpeg host upload | **NOT RUN** |
| VA-API VP9 Profile 0 | NV12 DMA-BUF zero-copy → WGPU, stable playback | **NOT RUN** |
| VA-API VP9 Profile 2 HDR | P010 DMA-BUF, HDR metadata и HDR-to-SDR | **NOT RUN** |
| Hardware AV1 rejection | typed `UnsupportedVideoCodec`, без software fallback при `hardware` | **NOT RUN** |
| H.264 MP4/MKV seek matrix | avcC, keyframes, B-frame PTS/DTS, signed ctts, startup/cues | **NOT RUN**; explicit scenarios доступны |
| H.265 MOV/MP4/MKV | hvcC, sync samples/cues, CRA/open-GOP, Main/Main10 | **NOT RUN** |
| DMA-BUF layouts Intel/AMD | composed/separate success; multi-object rejected before start | **NOT RUN** |
| Renderer recreation/device loss | exactly-once release, restore/rollback, no stale frame reuse | Unit contracts **PASS**, manual GPU **NOT RUN** |
| Audio | AAC-LC 5.1 channel order, iOS LPCM seek, Opus EOF/late seek, output device | Patch/unit **PASS**, real media/device **NOT RUN** |
| Direct HTTP Range | seekable direct media over Range | Hermetic test **PASS**, real media **NOT RUN** |
| YouTube Range/fallback/live/HDR | transport selection, live non-seekable path, typed HDR policy | Hermetic selection **PASS**, network/media **NOT RUN** |

Команды владельца matrix:

```text
scripts/runtime-acceptance.sh --suite runtime-software --vp9 <FILE> --h264 <FILE>
scripts/runtime-acceptance.sh --suite vaapi-hardware --vp9 <FILE> --av1 <FILE>
scripts/runtime-acceptance.sh --suite playback-matrix --vp9 <FILE> --av1 <FILE> --h264 <FILE>
scripts/media-regression.sh --scenario <NAME> --path <FILE>
```

## Разрешённые направления разработки

До зелёного повторного readiness audit разрешены:

- bounded исправление dependency advisories;
- bounded migration coverage policy/baseline;
- исправление smoke dry-run outcome vocabulary;
- crate-local panic follow-ups из `docs/panic-invariant-policy.md`;
- manual hardware/media acceptance без изменения product semantics.

После закрытия blocking gates новая разработка допустима только при соблюдении owner boundaries:

- playback/session — через `PlayerCommand`, worker/session boundary и focused lifecycle tests;
- media opening/network — внутри `service-direct-media`, `service-youtube`, `media-prefetch`, не в `player-core`;
- settings — через neutral `settings-core`, project routing `rustiplayer-settings` и app-owned runtime adapters;
- renderer/backend — через frame/backend/render contracts без concrete dependencies в neutral crates;
- codec support — внутри codec-owned parser/requirement/adapter modules с typed rejection и media matrix;
- новые hardware paths — только после заранее названных Intel/AMD/driver scenarios.

Hover preview/predecode/hover budget lanes остаются удалёнными и не являются разрешённым направлением без нового архитектурного решения владельца.

## Bounded follow-ups

- [Blocking quick-xml advisories](user/session_28_followup_dependency_advisories_2026-07-12.md)
- [Coverage baseline after decomposition](user/session_28_followup_coverage_baseline_after_decomposition_2026-07-12.md)
- [Unmaintained audiopus dependency](user/session_28_followup_audiopus_maintenance_2026-07-12.md)
- [Smoke dry-run outcome vocabulary](user/session_28_followup_smoke_dry_run_outcome_2026-07-12.md)

