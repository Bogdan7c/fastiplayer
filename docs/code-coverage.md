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

Hardware, FFI и UI-shell crate-ы перечислены в `informational_crates`. Их
результаты видны в `current-summary.json`, LCOV и HTML, но отдельная crate-метрика
пока не блокирует merge: hosted runner не доказывает реальную работу GPU,
VA-API, FFmpeg runtime, audio device или окна. При этом их измеренные файлы входят
в общий workspace ratchet, поэтому уже полученное hermetic покрытие нельзя
молча потерять.

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

Новый или изменённый boundary должен получить focused test. Исключение coverage
не заменяет такой тест и не оправдывает бессодержательные assertions ради
процента.

## Exclusions

Сейчас `excluded_source_paths` намеренно пуст:

- generated raw bindings local patch-crate `crates/cros-libva-patch/src/bindings.rs`
  находится вне workspace и поэтому не попадает в first-party report;
- Cargo/build output не включается `cargo-llvm-cov` по умолчанию, поскольку мы не
  передаём `--include-build-script`;
- manual hardware/runtime код не исключён из отчёта — он классифицирован как
  informational, чтобы непокрытые owner/error paths оставались видимыми.

Новое исключение допустимо только для действительно generated/raw binding или
ручного hardware-only исходника. Нужно добавить точный путь в policy и здесь
объяснить, кто генерирует код или почему hosted hermetic execution невозможен.
