# Измеряемая карта покрытия

## Что блокирует CI

Coverage здесь — ratchet риска, а не цель получить красивый общий процент.
`scripts/coverage.sh check` запускает ту же герметичную workspace suite, что и
основной test job: `--workspace --all-features --locked --no-fail-fast`.

В `coverage/baseline.json` хранятся только целочисленные пары `covered/total`
для lines, functions и regions:

- общий first-party workspace;
- агрегат pure contract/business группы;
- каждый pure contract/business crate из `coverage/policy.json`.

CI сравнивает дроби точно, без округления процентов. Уменьшение любой blocking
метрики запрещено. Произвольного global threshold нет: текущий измеренный
baseline и есть нижняя граница.

Фактический baseline после normalization migration от 2026-08-29:

- workspace: `157533/208133` lines, `15114/19430` functions,
  `195669/260067` regions;
- blocking group: `98718/116687` lines, `9698/11586` functions,
  `121984/146575` regions.

Это conservative per-metric envelope из двух реально наблюдавшихся clean runs:
scheduler-dependent worker tests могут менять, какой из соседних async paths
успевает получить execution counter. Для каждой пары `scope/metric` взят меньший
`covered` при одинаковом `total`; придуманные пороги и aggregate exceptions не
используются. После удаления 28 старых S42 transition rows остаются ровно восемь
normalization-only exceptions для `capability-core`, `hds-manifest-core` и
`service-direct-media`.

Последний clean gate artifact был не ниже baseline: workspace
`157537/208133` lines, `15114/19430` functions, `195669/260067` regions;
blocking group `98722/116687` lines, `9698/11586` functions,
`121984/146575` regions.

Hardware, FFI и UI-shell crate-ы перечислены в `informational_crates`. Их
результаты видны в `current-summary.json`, LCOV и HTML, но отдельная crate-метрика
пока не блокирует merge: hosted runner не доказывает реальную работу GPU,
VA-API, FFmpeg runtime, audio device или окна. При этом их измеренные файлы входят
в общий workspace ratchet, поэтому уже полученное hermetic покрытие нельзя
молча потерять.

`ui-artwork-egui` относится к informational UI surface: crate владеет отрисовкой
через `egui::Painter` и не является neutral business/contract boundary. Его
hermetic tests и production-файлы всё равно входят в общий workspace ratchet.

## Локальный запуск

Нужны exact tool и LLVM component:

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --version 0.8.7 --locked
```

Проверка относительно baseline:

```bash
scripts/coverage.sh check
```

Осознанное создание или обновление baseline:

```bash
scripts/coverage.sh baseline
```

Raw profile data остаются в `target/llvm-cov-target`. Summary JSON, LCOV и HTML
создаются в `target/coverage/`; Git их игнорирует, а CI загружает report вместе
с `*.profraw`/`*.profdata` как artifact `coverage-report`.

Перед публикацией HTML, записью baseline и blocking ratchet raw LCOV проходит
отдельную fail-closed проверку `DA`/`FNDA`/`BRDA` execution counters. Значение с
установленным старшим битом `u64` запрещено: оно указывает на underflow/corruption
LLVM counter-expression, например если test process завершился раньше detached
worker-а. Такой profile отклоняется как повреждённый и не может искусственно
повысить baseline.

Обычный `scripts/ci-checks.sh tests` запускается отдельно и не наследует
instrumented flags: `cargo-llvm-cov` задаёт их только дочерним процессам своего
вызова, а coverage runner начинает с собственного clean target.

## Осознанное уменьшение baseline

Править меньшие числа в `coverage/baseline.json` без записи в
`coverage/exceptions.json` запрещено. Для каждой уменьшенной пары `scope/metric`
exception обязана содержать:

- `scope`: `workspace`, `blocking-group` или точный `crate:<name>`;
- `metric`: `lines`, `functions` или `regions`;
- точные `previous` и `allowed` counters;
- конкретную `reason`;
- ISO-дату `review_by`, после которой запись перестаёт действовать;
- bounded `follow_up`: issue/задача с конечным результатом.

PR job сравнивает proposed baseline с baseline целевой ветки и блокирует любое
необъяснённое снижение. Exact counters не позволяют повторно использовать старое
или слишком широкое исключение.

Normalization migration содержит ровно восемь exact `scope/metric` записей:
`capability-core` lines/functions/regions, `hds-manifest-core` lines/regions и
`service-direct-media` lines/functions/regions. Они объясняют только смену
измеряемого source set после выноса тестов в диапазоне `39839dc3→857ac895`;
source, aggregate и остальные crate-ы исключений не имеют. У каждой записи
зафиксированы прежние и допустимые counters, paired clean evidence, bounded
follow-up и дата пересмотра `2026-10-25`. Это не новый общий порог и не
разрешение на дальнейшее снижение.

Lifecycle exception-ов проверяется при каждом обычном
`scripts/coverage.sh check` и `scripts/coverage.sh baseline`, а не только при
PR-сравнении двух baseline. Просроченная, дублированная или schema-invalid
запись блокирует coverage gate даже без нового уменьшения.

Новый или изменённый boundary должен получить focused test. Исключение coverage
не заменяет такой тест и не оправдывает бессодержательные assertions ради
процента.

## Exclusions

`excluded_source_paths` намеренно пуст и не добавляет project-specific
исключений. При этом `cargo-llvm-cov 0.8.7` применяет собственный default filename
regex: каталоги `tests`, `examples`, `benches`, а также файлы `tests.rs`,
`*_tests.rs` и `*-tests.rs` не входят в source report. Тестовые бинарники всё
равно исполняются; исключён только их исходный текст из знаменателей и execution
counters отчёта.

Дополнительно:

- generated raw bindings local patch-crate `crates/cros-libva-patch/src/bindings.rs`
  находится вне workspace и поэтому не попадает в first-party report;
- Cargo/build output не включается `cargo-llvm-cov` по умолчанию, поскольку мы не
  передаём `--include-build-script`;
- manual hardware/runtime код не исключён из отчёта — он классифицирован как
  informational, чтобы непокрытые owner/error paths оставались видимыми.

Новое исключение допустимо только для действительно generated/raw binding или
ручного hardware-only исходника. Нужно добавить точный путь в policy и здесь
объяснить, кто генерирует код или почему hosted hermetic execution невозможен.
