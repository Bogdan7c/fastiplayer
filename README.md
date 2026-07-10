# rustiplayer

`rustiplayer` — desktop-видеоплеер на Rust для Linux. Основной путь воспроизведения
использует VA-API decode и WGPU/Vulkan render; software fallback использует FFmpeg.
Проект находится в активной разработке и пока не является готовым portable release.

## Требования

- Linux x86_64 с Vulkan-capable GPU/driver;
- Rust toolchain `1.96.0` из `rust-toolchain.toml`; поддерживаемый MSRV — `1.92.0`;
- `clang`, `libclang`, `pkg-config`, development headers ALSA, FFmpeg, GBM и VA-API;
- `yt-dlp` в `PATH` только для открытия YouTube URL.

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

Разрешены crates.io, versioned workspace/path dependencies и четыре
инвентаризированных local patches. Новый Git source требует pinned `rev` и
документированных owner, причины и критерия удаления до изменения source policy.
Первый аудит и backlog находятся в
`docs/dependency-report-2026-07-10.md`.

## Runtime limitations

- CI компилирует код, но не проверяет реальный Vulkan/VA-API display, GPU import,
  аудиоустройство или воспроизведение media;
- hardware decode рассчитан на Linux VA-API; native Windows/macOS backends отсутствуют;
- software decode требует совместимые FFmpeg runtime libraries;
- native HDR output и CPU readback fallback не реализованы;
- DRM/codec support зависит от установленного GPU driver и FFmpeg build;
- YouTube playback зависит от внешнего `yt-dlp` и изменений сервиса.

Runtime acceptance выполняется отдельно:

```bash
scripts/playback-smoke.sh --mode probe-only
scripts/media-regression.sh --list-scenarios
```

## Лицензирование

First-party workspace code лицензирован под MIT, см. `LICENSE`. Первый commit и
текущий license change относятся к 2026 году, поэтому стандартная строка copyright
содержит один год: `2026 Bogdan7c`.

Четыре каталога `crates/*-patch` являются модифицированными upstream crates, не
first-party MIT-кодом. `cros-codecs` и `cros-libva` сохраняют BSD-3-Clause;
`symphonia-codec-aac` и `symphonia-format-isomp4` сохраняют MPL-2.0,
свои upstream license files/notices и file-level MPL obligations.
