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

Измеримый coverage ratchet запускается отдельно командой
`scripts/coverage.sh check`. Он сохраняет raw/LCOV/HTML только как CI artifact,
а в Git сравнивает компактный baseline workspace и pure contract/business
crate-ов. Политика, локальная установка exact tool и процедура осознанного
исключения описаны в `docs/code-coverage.md`. До baseline/ratchet runner
fail-closed отклоняет LCOV с top-bit execution counter corruption.

Семь local dependency patches остаются вне workspace и проверяются своими
manifest/lock парами. Их exact direct-команды и removal gates перечислены в
`docs/dependency-patches.toml`; workspace integration воспроизводится командой
`scripts/ci-checks.sh dependency-patches`, а локальный
`scripts/ci-checks.sh all` дополнительно запускает все семь standalone locked
suites. Local-media regressions получают только explicit `--scenario` +
`--path` через `scripts/media-regression.sh`; web-media manual acceptance
отдельно принимает только явно переданные `--case` + `--url`/`--fixture` через
`scripts/progressive-web-smoke.sh`.

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
- `Dependency policy`
- `Dependency patch (cros-libva)`
- `Dependency patch (cros-codecs)`
- `Dependency patch (symphonia-format-caf)`
- `Dependency patch (symphonia-format-isomp4)`
- `Dependency patch (symphonia-codec-aac)`
- `Dependency patch (symphonia-format-mkv)`
- `Dependency patch (wayland-scanner)`
- `Dependency patch integration`
- `Coverage ratchet`

Operational checklist:

1. Требовать pull request перед merge в `main`.
2. Требовать все шестнадцать status checks выше.
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

## Optional hardware/runtime regression smoke

GitHub-hosted clean runner не доказывает playback на реальном GPU/VA-API/audio.
Ручной workflow `.github/workflows/hardware-acceptance.yml` запускается только на
self-hosted runner-е с label `rustiplayer-hardware` и получает абсолютные пути к
реальным VP9, AV1 Main 8-bit SDR, отдельному AV1 Main 10-bit HDR и H.264
fixtures. Его job намеренно non-blocking.

Эквивалентная локальная acceptance-команда использует тот же repo runner:

```bash
scripts/playback-smoke.sh --mode full \
  --vp9 /absolute/path/to/vp9-profile0-4k60.webm \
  --av1 /absolute/path/to/av1-main-8bit-sdr-4k60.mp4 \
  --av1-hdr /absolute/path/to/av1-main-10bit-hdr-4k60.mp4 \
  --h264 /absolute/path/to/h264-4k60.mp4
```

Этот opt-in workflow запускает VP9/AV1/H.264 regression scenarios на конкретном
host и явно выбранных fixtures. Hardware preflight fail-closed требует readable
render node и exact `VAProfileAV1Profile0 : VAEntrypointVLD` в `vainfo`, иначе
suite возвращает reasoned `SKIP`, а не `PASS`. Оба AV1 hardware scenario требуют
`vaapi-dmabuf-wgpu`, configured AV1 adapter, первый NV12 для SDR или P010 для
HDR DMA-BUF и exact trace `video frame submitted to renderer`; FFmpeg fallback,
backend reselection и fatal markers запрещены. Full mode отдельно сохраняет
software AV1 SDR регрессию через `ffmpeg-host-upload-wgpu`.

Успешный результат относится только к этой host/fixture конфигурации и не
переписывает историческую S42 hardware acceptance.
Единственное owner-approved hardware-capability исключение S42 — exact
`VAProfileH264Baseline` → H.264 Baseline 8-bit YUV420/NV12, capability
intersection only; на момент S42 hardware manual rerun имел статус `NOT RUN`,
потому что у владельца тогда не было совместимого VA-API device. Текущая
AV1 Main SDR/HDR matrix является отдельной post-S42 feature acceptance.
