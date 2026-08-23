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
| optional VA-API regression smoke | `scripts/runtime-acceptance.sh --suite vaapi-hardware --vp9 <FILE> --av1 <FILE>` | Проверяет существующие VP9 VA-API DMA-BUF и typed AV1 rejection paths только на явно выбранном host/fixtures; не является общим S42 hardware acceptance | Явные files, readable render node, успешный `vainfo` |
| combined runtime regression set | `scripts/runtime-acceptance.sh --suite playback-matrix --vp9 <FILE> --av1 <FILE> --h264 <FILE>` | Запускает перечисленные software и VA-API scenarios; результат относится только к данному host/fixtures и не доказывает полную hardware matrix | Все перечисленные software/hardware prerequisites |

Отдельные команды, не смешанные с current playback config:

- `scripts/playback-smoke.sh --mode probe-only` — focused fake/unit probes и ignored real FFmpeg runtime probe.
- `scripts/ytdlp-compatibility.sh` — development-only проверка фактически
  найденного в `PATH` system `yt-dlp`: локальный HTTP fixture проходит через
  production candidate и topology API. Временный executable shim добавляет
  `--ignore-config --no-plugin-dirs` только этой проверке, чтобы user environment
  не маскировал upstream incompatibility. Номер версии выводится только как
  диагностическое свидетельство и не является allowlist/gate.
- `scripts/tests/ytdlp-compatibility-self-test.sh` — hermetic проверка CLI,
  exit-status и exact Cargo orchestration предыдущего runner-а.
- `scripts/playback-smoke.sh --mode legacy-migration` — явно выбранный legacy config migration smoke.
- `scripts/tests/playback-smoke-self-test.sh` — parser, dry-run и полный current-schema config generate/parse без GUI.
- `scripts/progressive-web-smoke.sh` — S42 manual opt-in только для явно
  переданных URL/fixtures; неполная matrix остаётся `NOT RUN`.
- [`web-media-playlist-acceptance.md`](web-media-playlist-acceptance.md) —
  отдельный ручной прогон одной смешанной XSPF-очереди по двенадцати крупным
  transport rows плюс полный сценарий вкладки настроек потока URL. Он удобен
  для последовательного поиска runtime-дефектов, но не заменяет 29-case S42
  topology/privacy checklist и сам по себе не является automated suite.
- `scripts/final-acceptance.sh` — полный automated S42 gate
  (`scripts/ci-checks.sh all` + `scripts/coverage.sh check`), без manual media.

## Инвентарь runtime/fixture/hardware тестов

По состоянию на S42 audit first-party inventory содержит:

- один ignored FFmpeg runtime probe: `video-ffmpeg/tests/ffmpeg_runtime_probe.rs`; запускается только через `probe-only`/software/full suite и требует установленный FFmpeg runtime;
- семнадцать ignored local-media demux regressions в `symphonia-demux/tests/`: шесть H.264, три H.265, один VP9, шесть audio и один generic inspection;
- один ignored direct HTTP Range regression в `service-direct-media`;
- один ignored system-`yt-dlp` regression
  `service-ytdlp/tests/system_ytdlp_compatibility.rs` запускается только через
  `scripts/ytdlp-compatibility.sh`; он использует loopback fixture и доказывает
  реальный candidate/topology process-parser-normalization path без внешнего URL;
- provider, auth, Range/refresh и live contracts по-прежнему проверяются
  hermetic fake/local-server suites, а real URL UX принадлежит только S42
  manual runner-у;
- VP9 VA-API playback и typed AV1 rejection runtime checks не являются
  `cargo test`: ими владеют `hardware-only` и `full` modes
  `playback-smoke.sh`; они требуют выбранных пользователем assets и конкретного
  GUI/VA-API/WGPU host и не являются checked-in S42 hardware acceptance.

Все fixture regressions запускаются по одному через `scripts/media-regression.sh --scenario <NAME> --path <FILE>`. Полный список требований доступен через `scripts/media-regression.sh --list-scenarios`; отсутствие selection уже печатает `NOT RUN`, а выбранный отсутствующий path завершает runner ошибкой.

Обычный `cargo test --workspace --all-features --locked` оставляет перечисленные runtime tests ignored. Сам по себе зелёный hermetic CI поэтому не является runtime-software, VA-API или playback acceptance.

Единственное owner-approved hardware-capability исключение, которое фиксирует
S42, — exact `VAProfileH264Baseline` → H.264 Baseline 8-bit YUV420/NV12,
capability intersection only. Более широкое hardware acceptance не заявлено;
current hardware manual rerun имеет статус `NOT RUN`: у владельца сейчас нет
совместимого VA-API device для opt-in rerun. Web-media manual status и полный
safe-case список описаны в
[web-media-s42-final-acceptance.md](web-media-s42-final-acceptance.md).
