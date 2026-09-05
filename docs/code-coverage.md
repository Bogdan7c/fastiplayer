# Стабильная карта покрытия исходного кода

## Ручной coverage и обязательная защита baseline

С 2026-09-05, по решению владельца, полное трёхпрогонное измерение вынесено
из обычного push/PR CI в `.github/workflows/coverage.yml` (`Coverage (manual)`).
Оно запускается через Actions → Coverage (manual) → Run workflow либо локальной
командой ниже. В обязательном CI остаётся быстрый `Coverage baseline policy`:
проверка schemas и PR previous/proposed pair без компиляции workspace.
Функциональные тесты и реальные FFmpeg/WGPU acceptance gates не отключались.
Термин blocking ratchet далее относится к exit status ручной команды: regression
по-прежнему завершает её с ошибкой. Успешный обычный CI сам по себе не означает
успешное новое измерение покрытия.

Coverage gate — это ratchet проверенного поведения, а не соревнование за общий
процент. Команда `scripts/coverage.sh check` строит одно instrumented workspace и
исполняет одну и ту же suite **ровно три раза** с обычной Cargo-конкурентностью:

```text
cargo test --workspace --all-features --locked --no-fail-fast
```

`RUST_TEST_THREADS` намеренно не фиксируется. Так gate не выдаёт искусственно
сериализованный режим за доказательство поведения обычного CI.

Для каждой source-coordinate — строки, функции или region — три запуска дают
один из трёх результатов:

- `stable`: coordinate исполнена в 3 из 3 запусков и участвует в blocking ratchet;
- `variable`: исполнена в 1 из 3 или 2 из 3 запусков и остаётся диагностикой;
- `uncovered`: исполнена в 0 из 3 запусков и остаётся видимым пробелом.

Variable coordinate не считается стабильной и не может подменить потерянную
stable coordinate. При неизменном source universe исчезновение любой конкретной
stable coordinate отклоняет ручную coverage-проверку, даже если другая coordinate стала covered и
общий процент выглядит тем же. При изменении source universe сравниваются exact
целочисленные пары `stable/total`; округлённые проценты в решении не участвуют.

Blocking domains задаются в `coverage/policy.json`:

- весь first-party workspace;
- агрегат contract/business crate-ов;
- каждый contract/business crate отдельно.

Hardware, FFI и UI-shell crate-ы перечислены в `informational_crates`. Их
отдельные crate-метрики не блокируют hosted CI, потому что runner не доказывает
работу реального GPU, VA-API, FFmpeg runtime, audio device или окна. Их исходники
всё равно входят в workspace domain, поэтому уже полученное hermetic покрытие
нельзя потерять молча.

## Versioned policy и provenance

Текущую blocking истину образуют четыре versioned файла:

- `coverage/policy.json` — source domains, tool version и source exclusions;
- `coverage/executable-inventory-policy.json` — typed runtime build roots и
  команды их materialization;
- `coverage/baseline.json` — stable-coordinate baseline schema v2;
- `coverage/measurement-exceptions.json` — exact provenance разрешённых
  cross-universe переходов.

Baseline хранит repo-relative source files, coordinate universes, stable sets,
целочисленные counts и SHA-256 каждого смыслового слоя. Toolchain, методика,
source inventory и policy hash входят в provenance. Несовпадение schema, hash,
tool identity или source-coordinate universe не превращается в нулевое покрытие,
а завершается отдельной fail-closed ошибкой.

`coverage/exceptions.json` больше не разрешает изменения blocking v2. Это
замороженное свидетельство старого ratio-based baseline v1: его содержимое и
восемь relocation/normalization identities встроены в
`baseline.legacy_report_only` и проверяются только как историческая provenance.
Legacy compact summary, LCOV и HTML также являются report-only diagnostics.

## Отдельный экспорт каждого executable

LLVM 22.1.2 может потерять выполненную функцию при одном общем export нескольких
ELF: для одних и тех же binaries и merged profile перестановка двух `-object`
воспроизводимо меняла count с 0 на 6. Поэтому blocking surface строится через
`coverage_object_export.py` и `coverage_object_union.py`:

- adapter получает exact object argv и source filters от pinned `cargo-llvm-cov`;
- каждый object сверяется с frozen parent/runtime executable inventory по SHA-256;
- каждый single-object JSON сохраняется с checksum в `objects/run-N/`;
- extractor отдельно валидирует topology, counts и source paths каждого JSON;
- coordinate universes и covered sets объединяются через set union внутри run;
  повторные копии одной функции не увеличивают denominator или execution counts;
- объединённый source inventory обязан совпасть с полным workspace inventory;
  пропущенный, изменённый или malformed export прерывает проверку;
- три measured run по-прежнему пересекаются: covered в одном run не означает stable.

Это исправление реализации прежнего правила объединения instantiations по exact
source coordinates; сами определения lines/functions/regions и schema v2 baseline
не меняются. `cohort-manifest.json.coordinate_export` явно фиксирует исправленный
метод и отделяет старый multi-object export как report-only. `raw/run-N.json`,
legacy summary, LCOV и HTML сохраняются для диагностики; blocking state теперь
вычисляется из полного набора `objects/run-N/*.json.gz`. Baseline update требует
обычного file-local review и квалификации девятью запусками, описанных ниже.

## Один build, typed prewarm и три запуска

Runner владеет полным lifecycle одного cohort:

1. Проверяет exact Rust, LLVM и `cargo-llvm-cov` versions.
2. Делает один full coverage clean и один workspace `--no-run` build.
3. Запускает typed prewarm для runtime-компиляторов из executable policy. Сейчас
   это `settings-derive` trybuild: сам test materialize-ит nested Cargo target,
   который невозможно честно получить одним `--no-run`.
4. Проверяет, что prewarm создал непустой набор profiles с exact именами, и
   удаляет эти profiles: prewarm не участвует в измерении.
5. После prewarm замораживает source, tool, parent executable и runtime-owned
   executable inventories.
6. Перед каждым из трёх measured runs очищает предыдущие profiles, выполняет
   одинаковую suite с normal concurrency, строит raw JSON/LCOV и извлекает
   source-coordinate state.
7. Пересекает три state в stable/variable/uncovered classes, валидирует LCOV
   counters и только затем атомарно публикует законченный cohort.

Run 3 profiles сохраняются как authoritative вход опубликованного merge; exact
список, размеры и SHA-256 записаны в cohort manifest. Profiles prewarm, run 1 и
run 2 после успеха не остаются. Это позволяет независимо воспроизвести final
LLVM report и одновременно запрещает смешивать профили разных запусков.

Перед публикацией raw LCOV проходит отдельную проверку `DA`/`FNDA`/`BRDA`
execution counters. Значение с установленным старшим битом `u64` означает
underflow/corruption LLVM counter expression и отклоняется, а не повышает
baseline искусственно.

## Executable inventory и граница crash recovery

Measured runs обязаны исполнять один смысловой instrumented build. Cohort
manifest schema v2 хранит два разных inventory:

- parent Cargo build: exact executable paths и semantic SHA-256; mode, size,
  device, inode, link count, `mtime` и `ctime` используются как дешёвые triggers
  повторного hash, а add/remove/symlink проверяются полным обходом;
- typed runtime roots: exact path set и полный SHA-256 проверяются после prewarm
  и после каждого measured run, даже если trybuild пересоздал inode или metadata.

Linux probe до измерения доказывает, что unprivileged write меняет `ctime`, в том
числе после восстановления `mtime`. Если такой probe недоступен или metadata
изменилась, runner делает полный semantic rehash вместо доверия metadata.
Symlink ancestors, escape за `CARGO_TARGET_DIR`, неожиданный executable и
исчезновение ожидаемого path завершают cohort ошибкой. Каждый hardlink alias
остаётся отдельным path в semantic inventory; dedup content hash не скрывает
добавление или удаление alias.

Существующий typed runtime root перед clean атомарно переносится в quarantine.
При обработанной ошибке текущего процесса runner удаляет только заново созданный
root и восстанавливает прежний. После успешной публикации quarantine переводится
в retired state и удаляется best-effort. Это process transaction, а не
power-loss transaction: directory `fsync` protocol намеренно не заявлен. Поэтому
непустой journal/quarantine после crash блокирует следующий запуск до ручного
расследования; runner не угадывает, какой cache считать истинным, и не стирает
его молча.

## Локальные команды

Нужны exact toolchain component и wrapper:

```bash
rustup component add llvm-tools-preview --toolchain 1.96.0
cargo +1.96.0 install cargo-llvm-cov --version 0.8.7 --locked
```

Обычная blocking проверка относительно tracked v2 baseline:

```bash
scripts/coverage.sh check
```

Тот же build-once/three-run cohort без baseline comparison:

```bash
scripts/coverage.sh report
```

Одноразовый migration v1 → v2 создаёт proposal под ignored `target/`, но никогда
не переписывает tracked baseline:

```bash
scripts/coverage.sh bootstrap \
  target/coverage/stable-baseline-v2-proposal.json
python3 scripts/coverage_stability.py validate \
  --kind baseline \
  --input target/coverage/stable-baseline-v2-proposal.json
```

`bootstrap` допустим только пока tracked baseline имеет schema v1. После
миграции изменение v2 — это reviewed изменение пары
`coverage/baseline.json` + `coverage/measurement-exceptions.json`; `check` и
`report` versioned policy не меняют.

Для обычной проверки достаточно одного cohort из трёх measured runs. Однако
обновление tracked baseline после правок конкурентных тестов квалифицируется
строже: на одной и той же source revision независимо собираются три cohort-а,
то есть девять measured workspace runs. В proposal попадает только точное
пересечение stable coordinates всех девяти запусков. Текущий CLI намеренно не
скрывает этот review за автоматическим cross-cohort reducer: reviewer отдельно
сверяет source/tool/profile и coordinate universes, а для каждого изменённого
файла проверяет stable counts. Aggregate workspace ratio не может доказать, что
потеря в одном файле случайно не замаскирована приростом в другом.

Executable SHA-256 обязан быть неизменным внутри каждого cohort-а: именно это
доказывает, что его три measured run исполняли один instrumented build. Между
независимыми cohort-ами байты ELF могут различаться из-за linker/compiler
nondeterminism; такое различие допустимо только при совпадающих source, exact
toolchain, profile и coordinate universe и при отдельно зелёной внутренней
валидации каждого cohort-а. Это не утверждение о reproducible builds.

После reviewed установки baseline выполняются два свежих
`scripts/coverage.sh check`. Оба обязаны вернуть пустые `regressions` и
`universe_changes`; retry не превращает реальный test/build failure в PASS.

Законченные artifacts находятся в `target/coverage/stable/`: три run state,
cohort, variable diagnostics, schema-v2 cohort manifest, raw summaries, LCOV,
HTML и legacy report-only files. CI публикует тот же стабильный artifact с именем
`coverage-report`.

Обычный `scripts/ci-checks.sh tests` запускается отдельно и не наследует
instrumented flags: `cargo-llvm-cov` передаёт wrapper environment только своим
дочерним процессам, а coverage runner владеет изолированным Cargo target и
profile directory.

Квалификация исправления S12 с тремя независимыми cohort и проверкой по файлам:
[отчёт 2026-09-05](coverage-qualification-2026-09-05.md).

## Осознанное обновление v2 baseline

PR job читает предыдущую пару JSON непосредственно из base-ветки и запускает
новый read-only validator из текущего commit:

```bash
python3 scripts/coverage_stability.py check-baseline-update \
  --previous-baseline /tmp/coverage-previous-baseline.json \
  --previous-measurement-exceptions /tmp/coverage-previous-measurement-exceptions.json \
  --proposed-baseline coverage/baseline.json \
  --proposed-measurement-exceptions coverage/measurement-exceptions.json
```

Все четыре аргумента обязательны. Exit `0` означает допустимое изменение, `1` —
валидный, но запрещённый semantic transition, `2` — malformed schema, hash,
redaction или I/O failure. Validator ничего не записывает.

Правила перехода намеренно узкие:

- потерю exact stable coordinate в том же universe исключить нельзя;
- смена source/policy universe требует явного baseline update;
- снижение cross-universe дроби `stable/total` разрешается только новой exact
  строкой в proposed measurement exceptions;
- строка содержит domain/metric, прежние и разрешённые counts, прежний и новый
  universe SHA-256, конкретную причину, `review_by` и bounded `follow_up`;
- proposed rows должны ровно соответствовать всем реально потреблённым потерям;
  stale, duplicate, unknown или expired rows запрещены;
- previous exception rows являются provenance предыдущего baseline и никогда не
  авторизуют новое снижение;
- истёкшая previous row остаётся читаемой только для recovery: реальный baseline
  transition обязан заменить её свежим exact ledger; отдельный PR без изменения
  blocking baseline не может удалить, продлить или переписать history;
- при неизменном baseline proposed exception document обязан быть семантически
  идентичен предыдущему.

Таким образом `measurement-exceptions.json` сохраняет immutable provenance
перехода, породившего текущий baseline, но не становится запасом разрешений на
будущие регрессии. Обычный `scripts/coverage.sh check` применяет deadline к
текущему ledger, поэтому просрочка блокирует gate до настоящего reviewed
baseline transition. Новый boundary всё равно обязан получить functional test;
coverage exception не заменяет проверку поведения.

## Source exclusions

`excluded_source_paths` намеренно пуст. `cargo-llvm-cov 0.8.7` применяет свой
default filename regex: каталоги `tests`, `examples`, `benches`, а также файлы
`tests.rs`, `*_tests.rs` и `*-tests.rs` не входят в source report. Тестовые
бинарники исполняются; исключён только их исходный текст из source denominator.

Дополнительно:

- generated bindings `crates/cros-libva-patch/src/bindings.rs` находятся вне
  workspace;
- Cargo/build output не включается без `--include-build-script`;
- manual hardware/runtime код не исключён: он informational, чтобы его
  непокрытые owner/error paths оставались видимыми.

Новое исключение допустимо только для действительно generated binding или
ручного hardware-only исходника. В policy нужен exact path, а здесь — владелец
генерации либо причина, почему hosted hermetic execution невозможен.
