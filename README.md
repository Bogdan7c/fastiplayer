# rustiplayer

`rustiplayer` — desktop-видеоплеер на Rust для Linux. Основной путь воспроизведения
использует VA-API decode и WGPU/Vulkan render; software fallback использует FFmpeg.
Проект находится в активной разработке и пока не является готовым portable release.

## Требования

- Linux x86_64 с Vulkan-capable GPU/driver;
- Rust toolchain `1.96.0` из `rust-toolchain.toml`; поддерживаемый MSRV — `1.92.0`;
- `clang`, `libclang`, `pkg-config`, development headers ALSA, FFmpeg, GBM и VA-API;
- `yt-dlp 2026.07.04` в `PATH` для утверждённого web-media profile. Другой
  release не принимается S42 manual acceptance как совместимый.

Для Ubuntu 24.04 compile dependencies соответствуют CI:

```bash
sudo apt-get install clang libclang-dev libasound2-dev libavcodec-dev \
  libavutil-dev libgbm-dev libva-dev pkg-config
```

## Сборка и проверки

```bash
cargo +1.96.0 build --workspace --locked
cargo +1.96.0 test --workspace --locked
scripts/pre-pr-checks.sh
```

Default feature приложения компилирует FFmpeg software decode. Проверить feature-off
boundary можно командой:

```bash
cargo +1.96.0 check -p app-egui --no-default-features --locked
```

Запуск локального файла:

```bash
cargo run -p app-egui -- /path/to/media.mp4
```

## Dependency policy

Основной policy tool — `cargo-deny 0.20.2`: он проверяет RustSec advisories,
licenses, sources и показывает duplicate versions. `cargo-machete 0.9.2` выполняет
только отдельную проверку unused direct dependencies, потому что cargo-deny не
анализирует использование crate в исходном коде.

```bash
cargo install cargo-deny --version 0.20.2 --locked
cargo install cargo-machete --version 0.9.2 --locked
scripts/ci-checks.sh dependencies
```

Vulnerability и unsound advisories блокируют gate. Yanked и unmaintained findings
всегда публикуются, но сами по себе не блокируют. В `cargo-deny 0.20` нет уровня
`warn` для unmaintained, поэтому `deny.warnings.toml` запускается как отдельный
non-blocking report после blocking policy из `deny.toml`.

Разрешены crates.io, versioned workspace/path dependencies и семь
инвентаризированных local patches. Новый Git source требует pinned `rev` и
документированных owner, причины и критерия удаления до изменения source policy.
Первый аудит и backlog находятся в
`docs/dependency-report-2026-07-10.md`.

## Web media

URL service принимает exact HTTP, HTTPS, FTP и FTPS locator-ы. Утверждённый
runtime profile включает progressive HTTP/FTP, HLS VOD/live/DVR, DASH
VOD/live/DVR, static ISM/MSS H.264+AAC VOD и static HDS/F4M/F4F VOD. Поддержка
ограничена exact containers/codecs/profile rows; произвольный результат
extractor-а не становится совместимым автоматически.

Playlist layer поддерживает M3U/M3U8, XSPF и CUE import/export. Public
single/playlist/channel/search и `multi_video` topology проходят bounded
preview/commit path. `multi_video` остаётся одной first-class top-level Group
entry с part-level navigation.

RTSP/RTP/MMS, RTMP wire playback, private live extractor state и DRM явно
исключены. ISM и HDS заявлены только как static VOD; subtitle descriptors не
означают subtitle playback. Точная matrix:
[web-media compatibility](docs/web-media-compatibility-matrix.md).

Production extraction сохраняет обычный system/user `yt-dlp` config, plugins и
cookies как manual opt-in trust boundary. Rustiplayer-owned arguments не
добавляют download/write/exec/postprocessor/mark-watched options и приложение не
сохраняет app-owned browser/cookie credentials. Exact locator, который
пользователь явно подтвердил, остаётся durable reopen identity, поэтому
credential-like material внутри самого locator также сохраняется; transient
headers, cookies и resolved targets в playlist state не попадают. User
config/plugins являются trusted external code; их side effects находятся вне
app guarantee.

## Runtime limitations

- CI компилирует код, но не проверяет реальный Vulkan/VA-API display, GPU import,
  аудиоустройство или воспроизведение media;
- hardware decode рассчитан на Linux VA-API; native Windows/macOS backends отсутствуют;
- software decode требует совместимые FFmpeg runtime libraries;
- native HDR output и CPU readback fallback не реализованы;
- hardware/software codec availability зависит от GPU driver и FFmpeg build;
- DRM playback не реализован;
- generic web URL playback зависит от exact `yt-dlp 2026.07.04`, extractor-а,
  доступности сервера и совпадения результата с утверждённой compatibility matrix;
- FFmpeg используется только как software decoder, не как hidden network,
  demux или RTMP fallback.

В S42 hardware claim ограничен owner-approved S27 exception: exact
`VAProfileH264Baseline` → H.264 Baseline 8-bit YUV420/NV12, capability
intersection only. Более широкое hardware acceptance не заявлено; current
hardware manual rerun имеет статус `NOT RUN`: у владельца сейчас нет
совместимого VA-API device для opt-in rerun.

Runtime acceptance выполняется отдельно:

```bash
scripts/final-acceptance.sh
scripts/playback-smoke.sh --mode probe-only
scripts/media-regression.sh --list-scenarios
scripts/progressive-web-smoke.sh --help
```

Последний S42 automated run от 2026-07-25 завершён `PASS`: primary Rust 1.96.0,
locked MSRV 1.92, hermetic suites, strict Clippy/rustdoc/fmt, dependency
inventory, guardrails и coverage ratchet прошли.

Automated S42 gate не запускает пользовательские URL/fixtures. Manual часть
остаётся `NOT RUN`, пока пользователь явно не передал полный corpus и не
проверил generated checklist:
[S42 final acceptance](docs/web-media-s42-final-acceptance.md). Значения
operational errors и безопасные действия описаны в
[web-media operational errors](docs/web-media-operational-errors.md).
Scoped profile trace хранится в `final-acceptance-s42.json`, а полный
machine-readable §14 goal→code/tests trace — отдельно в
`roadmap-trace-s42.json`; оба проверяются hermetic Cargo target-ом и не
подменяют manual acceptance.

## Лицензирование

First-party workspace code лицензирован под MIT, см. `LICENSE`. Первый commit и
текущий license change относятся к 2026 году, поэтому стандартная строка copyright
содержит один год: `2026 Bogdan7c`.

Семь каталогов `crates/*-patch` являются модифицированными upstream crates, не
first-party MIT-кодом. `cros-codecs` и `cros-libva` сохраняют BSD-3-Clause;
`symphonia-codec-aac`, `symphonia-format-caf`,
`symphonia-format-isomp4` и `symphonia-format-mkv` сохраняют MPL-2.0;
`wayland-scanner` сохраняет MIT. Каждый patch сохраняет собственные upstream
license files/notices и соответствующие file-level obligations.
