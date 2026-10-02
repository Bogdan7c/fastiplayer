# Panic и invariant policy

Production-код не должен паниковать на границах, где отказ зависит от среды или
внешнего состояния. Ошибки OS, thread spawn/join, mutex/RwLock, файлов, сети,
декодируемого input и пользовательской конфигурации возвращаются как typed error
через boundary владельца состояния. Poisoned lock не восстанавливается через
`into_inner()`, если poison означает потерю доверия к защищаемым инвариантам.

`expect` допустим только для compile-time/non-zero констант и для доказанного
private invariant. Рядом с таким `expect` должно быть локальное объяснение: кто
создал invariant и почему внешний input не может его нарушить. Проверка
`is_some()` перед `take().expect(...)` не считается хорошим доказательством:
предпочтителен structural `match`/`let Some`, который сохраняет ownership и явно
описывает невозможную или fallible ветку.

## Машинная проверка

Политику проверяет Clippy через `[workspace.lints.clippy]` в корневом
`Cargo.toml`: `unwrap_used`, `expect_used` и `panic` включены как `warn`, а CI
запускает Clippy с `-D warnings`, поэтому новое нарушение ломает сборку. Каждый
first-party crate подключает правила через `[lints] workspace = true`.

- Тесты освобождены через `clippy.toml` (`allow-*-in-tests`). Интеграционные
  тесты в `tests/` объявляют это явно атрибутом в корне каждого test crate-а.
  Модули с составным cfg объявляются как `#[cfg(test)]` + `#[cfg(unix)]`, чтобы
  Clippy распознал их как тестовые.
- Ненулевые константы вычисляются в `const { ... }`: ноль становится ошибкой
  компиляции, а не паникой во время работы.
- Если тип уже хранит `NonZero*`, наружу отдаётся метод вроде `non_zero()`,
  чтобы вызывающий код не проверял ноль повторно.
- Допустимая паника оформляется локально как
  `#[expect(clippy::expect_used, reason = "...")]` на минимальном scope:
  `let`-операторе или функции. В `reason` записано, почему инвариант
  выполняется. Атрибут `expect` (а не `allow`) сам сообщит, если паника
  исчезнет и обоснование станет лишним.
- Перед аннотацией сначала пробуем убрать причину: `take_if`, `next_if`,
  `get_or_insert_with`, `Option::insert`, массивы через `first_chunk`, enum вместо
  нескольких согласованных `Option`.

Аудит текущего состояния:

```text
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

## Исторический baseline Сессии 11

Сохранён как история. С 2026-10-02 все группы ниже закрыты: места либо
переписаны структурно, либо получили локальный `#[expect(..., reason)]`, а
новые нарушения ловит линтер (см. «Машинная проверка»).

Команда аудита:

```text
cargo clippy --workspace --lib -- -W clippy::unwrap_used -W clippy::expect_used
```

До правок: 53 finding-а — 2 `unwrap` и 51 `expect`. После bounded исправлений:
38 `expect`, runtime `unwrap` отсутствуют. Оставшиеся findings не подавлены и
разделены на follow-up группы ниже.

## Follow-up группы

- `media-prefetch` — 9: отдельно проверить arithmetic overflow/underflow буфера,
  преобразования `u64`/`usize` на поддерживаемых платформах и размеры allocation;
  fallible config/input пути должны вернуть typed error, а доказанные счётчики
  должны получить локально сформулированный invariant.
- `player-core` — 7: три private invariants непустого audio-clock mapping и одна
  non-zero `TimeBase` константа; отдельно протянуть ошибки default/validated
  frame-server config (3) через startup/config boundary вместо panic.
- `render-wgpu-video` — 7: четыре доказуемых non-zero buffer/alignment constants;
  отдельно проверить private pending-upload encoder invariant и два validated
  YUV frame-field invariant, сохранив renderer error semantics.
- `settings-derive` — 4: validated proc-macro option paths должны либо остаться
  локально доказанными codegen invariants, либо возвращать `syn::Error` на macro
  input без panic; исправлять одним crate-local work package.
- `frame-server-core` — 3: два валидных `Default` config invariant и overflow
  scrub generation; overflow является runtime lifecycle boundary и требует
  отдельного typed outcome без изменения latest-only semantics.
- `symphonia-demux` — 2: non-zero default corrupted-packet constant допустим;
  `unsupported_kind` требует structural mapping либо локального доказательства
  TrackEntry invariant.
- `video-ffmpeg` — 2: compile-time backend id и validated pixel-layout pair;
  первый допустим как constant invariant, второй проверить внутри codec-adapter
  error boundary.
- `audio-core` — 1: доказать непустой canonical positional layout около lookup
  либо сделать structural iteration без изменения channel order.
- `video-core` — 1: literal `1` является non-zero constant invariant; заменить
  на именованную константу/безопасную конструкцию только локально.
- `service-ytdlp` — 1: validated user config остаётся input boundary и должен
  возвращать typed mapping error вместо panic.
- `video-vaapi` — 1: literal `1` в surface accounting является non-zero constant
  invariant; resource-pool poison findings в этой группе закрыты Сессией 11.

Каждая группа выполняется отдельно, с focused tests владельца boundary; этот
реестр не является разрешением на механическую workspace-wide замену `expect`.
