# Runtime acceptance manifest

Единая executable-точка входа — `scripts/runtime-acceptance.sh`. Её `--help` перечисляет четыре suite и outcome contract. Скрипт не ищет файлы в `test-assets/` и не угадывает локальное окружение.

## Outcome contract

- `PASS` — команда действительно выполнена, assertions прошли, exit code `0`.
- `SKIP: <причина>; acceptance not satisfied` — явно выбранной suite не хватает fixture/runtime/hardware prerequisite, exit code `3`. Это не выполненная acceptance.
- `NOT RUN: <причина>; acceptance not satisfied` — suite не выбрана или вызван `--dry-run`. Команды не выполнялись, acceptance не выполнена.
- `FAIL` — выбранная команда или assertion завершились ошибкой; сохраняется её ненулевой exit code.

## Manifest команд

| Suite | Команда | Что доказывает | Требования |
|---|---|---|---|
| hermetic CI | `scripts/runtime-acceptance.sh --suite hermetic-ci` | All-features workspace tests без owner-local media | Cargo/toolchain и зависимости CI |
| runtime software | `scripts/runtime-acceptance.sh --suite runtime-software --vp9 <FILE> --h264 <FILE>` | FFmpeg runtime probe, software H.264 playback, VP9 stress | Явные files, FFmpeg `libavcodec >= 62`, `libavutil >= 60` |
| VA-API hardware | `scripts/runtime-acceptance.sh --suite vaapi-hardware --vp9 <FILE> --av1 <FILE>` | VP9 VA-API DMA-BUF playback и typed AV1 hardware rejection | Явные files, readable render node, успешный `vainfo` |
| playback matrix | `scripts/runtime-acceptance.sh --suite playback-matrix --vp9 <FILE> --av1 <FILE> --h264 <FILE>` | Полная hardware/software playback matrix | Все перечисленные software/hardware prerequisites |

Отдельные команды, не смешанные с current playback config:

- `scripts/playback-smoke.sh --mode probe-only` — focused fake/unit probes и ignored real FFmpeg runtime probe.
- `scripts/playback-smoke.sh --mode legacy-migration` — явно выбранный legacy config migration smoke.
- `scripts/tests/playback-smoke-self-test.sh` — parser, dry-run и полный current-schema config generate/parse без GUI.

## Инвентарь runtime/fixture/hardware тестов

По состоянию на Сессию 17 first-party inventory содержит:

- один ignored FFmpeg runtime probe: `video-ffmpeg/tests/ffmpeg_runtime_probe.rs`; запускается только через `probe-only`/software/full suite и требует установленный FFmpeg runtime;
- семнадцать ignored local-media demux regressions в `symphonia-demux/tests/`: шесть H.264, три H.265, один VP9, шесть audio и один generic inspection;
- один ignored direct HTTP Range regression в `service-direct-media`;
- четыре ignored `yt-dlp`/network regressions в `service-ytdlp`: explicit
  non-YouTube URL smoke, Range, fallback и live source;
- hardware playback assertions не являются `cargo test`: ими владеют `hardware-only` и `full` modes `playback-smoke.sh`, потому что им нужны GUI/runtime, VA-API/WGPU и выбранные пользователем assets.

Все fixture regressions запускаются по одному через `scripts/media-regression.sh --scenario <NAME> --path <FILE>`. Полный список требований доступен через `scripts/media-regression.sh --list-scenarios`; отсутствие selection уже печатает `NOT RUN`, а выбранный отсутствующий path завершает runner ошибкой.

Обычный `cargo test --workspace --all-features --locked` оставляет перечисленные runtime tests ignored. Сам по себе зелёный hermetic CI поэтому не является runtime-software, VA-API или playback acceptance.
