# Continuous integration и required checks

Физические GBM/VA-API и DMA-heap тесты выполняются только локально.
`CI` явно задаёт `FASTIPLAYER_TEST_SCOPE=hosted` и фильтрует пять аппаратных
tests до выполнения (`scripts/test_execution_scope.py`). Native headers, SDK
integration, программные VA-API tests и FFmpeg/WGPU через software adapter
остаются удалёнными.

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

Coverage-gate (stable-coordinate ratchet, baseline, manual workflow
`Coverage (manual)` и job `Coverage baseline policy`) удалён по решению владельца
2026-10-03: покрытие показывает лишь, что строка выполнилась, а не что тест
проверил результат. Качество тестов задаётся правилом в `AGENTS.md` («тест
проверяет результат, а не вызов») и функциональными тестами. Посмотреть покрытие
для себя можно вручную: `cargo llvm-cov --workspace --all-features --html`.

Семь local dependency patches остаются вне workspace и проверяются своими
manifest/lock парами. Их exact direct-команды и removal gates перечислены в
`docs/dependency-patches.toml`; workspace integration воспроизводится командой
`scripts/ci-checks.sh dependency-patches`, а локальный
`scripts/ci-checks.sh all` дополнительно запускает все семь standalone locked
suites. Local-media regressions получают только explicit `--scenario` +
`--path` через `scripts/media-regression.sh`; web-media manual acceptance
отдельно принимает только явно переданные `--case` + `--url`/`--fixture` через
`scripts/progressive-web-smoke.sh`.

All-target job `Workspace tests (all features)` на clean Ubuntu 24.04 runner
явно устанавливает только native build dependencies:
`clang`, `libclang-dev`, `libasound2-dev`, `libavcodec-dev`, `libavutil-dev`,
`libgbm-dev`, `libsoundtouch-dev`, `libva-dev`, `libvulkan1`,
`mesa-vulkan-drivers` и `pkg-config`. Они нужны для bindgen, CPAL/ALSA, FFmpeg,
GBM, VA-API, линковки SoundTouch backend example и headless lavapipe adapter.
Job задаёт job-local `CARGO_PROFILE_TEST_DEBUG=0`: состав тестов сохраняется, а полный DWARF не переполняет диск hosted runner до
запуска test binaries. Остальные jobs и локальные Cargo profiles не меняются.
WGPU acceptance в этом job выполняется через software Vulkan/lavapipe; реальный
GPU, VA display, звуковое устройство и окно не требуются и не эмулируются.

Toolchain-policy matrix устанавливает тот же workspace minimum явно, включая
прямой `libdrm-dev`: ALSA, libavcodec/libavutil, VA-API/DRM/GBM,
clang/libclang и pkg-config. `Format and guardrails` отдельно устанавливает
exact Rust 1.99.0 + `rustfmt`, а `Strict Clippy` — exact Rust 1.99.0 +
`clippy`, поэтому quality jobs не зависят от состава runner tool cache.

Совместимость cros-libva с системными headers проверяется по обе стороны
границы VA-API 1.23. Ubuntu 24.04 job утверждает API 1.20 и отсутствие новых
VP9 fields; Ubuntu 26.04 job утверждает API 1.23 и наличие обоих fields. После
проверки header каждый job запускает полный standalone locked crate test/build.


Operational checklist:

1. Требовать pull request перед merge в `main`.
2. Требовать все семнадцать status checks из `.github/workflows/ci.yml`.
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
self-hosted runner-е с label `fastiplayer-hardware` и получает абсолютные пути к
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
