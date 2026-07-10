# Continuous integration и required checks

## Единый источник команд

Blocking workflow и локальная проверка вызывают один repo runner:

```bash
scripts/ci-checks.sh all
```

Совместимая команда перед pull request остаётся такой:

```bash
scripts/pre-pr-checks.sh
```

Отдельный CI job можно воспроизвести, передав runner-у имя проверки из
`scripts/ci-checks.sh --help`. Все Cargo-команды используют `--locked`.

Четыре local dependency patches остаются вне workspace и проверяются своими
manifest/lock парами. Их exact direct-команды и removal gates перечислены в
`docs/dependency-patches.toml`; workspace integration воспроизводится командой
`scripts/ci-checks.sh dependency-patches`. Реальные media cases по-прежнему
получают только явный local path через `scripts/media-regression.sh`.

Clean Ubuntu 24.04 runner явно устанавливает только native build dependencies:
`clang`, `libclang-dev`, `libasound2-dev`, `libavcodec-dev`, `libavutil-dev`,
`libgbm-dev`, `libva-dev` и `pkg-config`. Они нужны для bindgen, CPAL/ALSA,
FFmpeg, GBM и VA-API compile/link paths. WGPU/Vulkan в blocking CI только компилируется: GPU, VA
display, звуковое устройство и окно не требуются и не эмулируются.

## Будущие required checks для main

Сейчас репозиторий намеренно остаётся приватным без GitHub Pro. На этом тарифе
GitHub не предоставляет rulesets/branch protection для данного репозитория,
поэтому принудительная блокировка merge отключена решением владельца. CI при
этом продолжает запускаться и показывать failures, но не является техническим
запретом на merge.

Когда репозиторий станет публичным, после первого успешного запуска
`.github/workflows/ci.yml` нужно настроить ruleset или branch protection для
`main`. Обязательными должны стать следующие точные status check names:

- `Format and guardrails`
- `Strict Clippy`
- `Documentation`
- `Workspace tests (all features)`
- `app-egui (no default features)`
- `MSRV (Rust 1.92.0)`
- `Dependency patch (cros-libva)`
- `Dependency patch (cros-codecs)`
- `Dependency patch (symphonia-format-isomp4)`
- `Dependency patch (symphonia-codec-aac)`
- `Dependency patch integration`

Operational checklist:

1. Требовать pull request перед merge в `main`.
2. Требовать все одиннадцать status checks выше.
3. Требовать актуальную ветку перед merge (`Require branches to be up to date`).
4. Запретить merge при failed, pending или stale required checks.
5. Не добавлять `Real playback smoke (manual, non-blocking)` в required checks.
6. Проверить настройки отдельным pull request с заведомо сломанной проверкой,
   затем удалить тестовую поломку.

Сам файл workflow не блокирует merge. В текущем приватном режиме контроль
failures выполняется человеком; автоматическое enforcement отложено до будущей
публикации репозитория.

Для приватного репозитория GitHub может требовать платный тариф владельца для
rulesets/branch protection. Ответ API `403 Upgrade to GitHub Pro or make this
repository public` означает ограничение тарифа, а не ошибку workflow или token
scope. В таком состоянии CI показывает failures, но технически запретить merge
в `main` не может. Это принятая текущая limitation, а не скрытая гарантия.

## Hardware/runtime acceptance

GitHub-hosted clean runner не доказывает playback на реальном GPU/VA-API/audio.
Ручной workflow `.github/workflows/hardware-acceptance.yml` запускается только на
self-hosted runner-е с label `rustiplayer-hardware` и получает абсолютные пути к
реальным VP9, AV1 и H.264 fixtures. Его job намеренно non-blocking.

Эквивалентная локальная acceptance-команда использует тот же repo runner:

```bash
scripts/playback-smoke.sh --mode full \
  --vp9 /absolute/path/to/vp9-profile0-4k60.webm \
  --av1 /absolute/path/to/av1-4k60.mp4 \
  --h264 /absolute/path/to/h264-4k60.mp4
```

Это настоящий runtime smoke; software stub не заменяет hardware path.
