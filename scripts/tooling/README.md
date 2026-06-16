# FFmpeg LGPL Build Tooling

Этот каталог содержит tooling для локальной сборки dynamic LGPL FFmpeg/libav*
под optional software decode backend. Сам tooling не делает запуск плеера
зависимым от FFmpeg; workspace crate `video-ffmpeg` подключает raw binding
только за explicit Cargo feature `ffmpeg`.

## Архитектурная граница

- `scripts/tooling/build-ffmpeg-lgpl.sh` владеет только download/configure/make/install workflow.
- Runtime capability проверяется optional probe-ом в `video-ffmpeg`: отсутствие
  FFmpeg не должно мешать старту `rustiplayer`.
- Demuxing остаётся в существующих media crates; этот tooling не собирает `libavformat`.
- CPU conversion не становится playback path: `libswscale` и `libswresample` выключены по умолчанию и включаются только явным opt-in для будущих header/build проверок.
- FFmpeg hardware acceleration не используется: native hardware path остаётся за текущими backend crates.

Context7 был использован перед добавлением tooling:

- `/websites/ffmpeg_documentation` для configure/build assumptions и состава FFmpeg libraries.
- `/websites/ffmpeg_doxygen_trunk` для сверки trunk headers/library assumptions.
- `/websites/ffmpeg_doxygen_8_0` для сверки stable 8.x headers/library assumptions.

Официальный release index FFmpeg на 2026-06-16 содержит `ffmpeg-8.1.1.tar.xz`,
поэтому default version зафиксирован как `8.1.1` внутри ветки 8.1.x.

## Быстрый старт

Проверить CLI без побочных эффектов:

```bash
scripts/tooling/build-ffmpeg-lgpl.sh --help
scripts/tooling/build-ffmpeg-lgpl.sh --dry-run
```

Собрать в default prefix внутри ignored `target/`:

```bash
scripts/tooling/build-ffmpeg-lgpl.sh
```

Собрать в явный prefix, например системный локальный каталог:

```bash
scripts/tooling/build-ffmpeg-lgpl.sh --prefix /rustiplayer-ffmpeg
```

Если архив уже скачан и сеть не нужна:

```bash
scripts/tooling/build-ffmpeg-lgpl.sh \
  --source-archive /path/to/ffmpeg-8.1.1.tar.xz
```

Если исходники уже распакованы:

```bash
scripts/tooling/build-ffmpeg-lgpl.sh \
  --source-dir /path/to/ffmpeg-8.1.1
```

## Env Vars

- `RUSTIPLAYER_FFMPEG_VERSION` - версия FFmpeg stable 8.1.x, default `8.1.1`.
- `RUSTIPLAYER_FFMPEG_PREFIX` - install prefix, default `target/rustiplayer-ffmpeg/<version>`.
- `RUSTIPLAYER_FFMPEG_WORK_DIR` - каталог downloads/source/build cache, default `target/rustiplayer-ffmpeg/build`.
- `RUSTIPLAYER_FFMPEG_SOURCE_DIR` - уже распакованный source tree с `configure`.
- `RUSTIPLAYER_FFMPEG_SOURCE_ARCHIVE` - локальный `ffmpeg-<version>.tar.xz`.
- `RUSTIPLAYER_FFMPEG_URL` - mirror URL для source archive.
- `RUSTIPLAYER_FFMPEG_JOBS` - число parallel `make` jobs.
- `RUSTIPLAYER_FFMPEG_ENABLE_SWRESAMPLE` - `0`/`1`, default `0`.
- `RUSTIPLAYER_FFMPEG_ENABLE_SWSCALE` - `0`/`1`, default `0`.

После установки explicit FFmpeg build/runtime проверки должны смотреть в
локальный prefix явно:

```bash
export FFMPEG_DIR="$RUSTIPLAYER_FFMPEG_PREFIX"
export PKG_CONFIG_PATH="$RUSTIPLAYER_FFMPEG_PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LD_LIBRARY_PATH="$RUSTIPLAYER_FFMPEG_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
```

`FFMPEG_DIR` нужен `ffmpeg-sys-next` при explicit prefix-е. `PKG_CONFIG_PATH`
нужен для pkg-config lookup. `LD_LIBRARY_PATH` нужен только для локального
запуска binaries/tests, которые dynamic-link к этим libraries. Обычный
`cargo check --workspace` не включает feature `ffmpeg`, поэтому эти export не
нужны для default workspace build.

Проверить explicit FFmpeg build path:

```bash
FFMPEG_DIR="$RUSTIPLAYER_FFMPEG_PREFIX" \
PKG_CONFIG_PATH="$RUSTIPLAYER_FFMPEG_PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
LD_LIBRARY_PATH="$RUSTIPLAYER_FFMPEG_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
cargo check -p video-ffmpeg --features ffmpeg
```

Проверить real runtime probe, если local FFmpeg runtime установлен:

```bash
FFMPEG_DIR="$RUSTIPLAYER_FFMPEG_PREFIX" \
PKG_CONFIG_PATH="$RUSTIPLAYER_FFMPEG_PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
LD_LIBRARY_PATH="$RUSTIPLAYER_FFMPEG_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
cargo test -p video-ffmpeg --features ffmpeg -- --ignored
```

## Guardrail

Сборка FFmpeg в локальный prefix не означает, что плеер зависит от FFmpeg при
старте. До отдельного архитектурного решения запрещено:

- добавлять `ffmpeg-*`, `libav*`, `rsmpeg` или похожие crates вне
  `video-ffmpeg`;
- включать FFmpeg feature в default workspace/app build;
- добавлять public `ffmpeg_sw`/`ffmpeg-sw` config или UI option;
- использовать FFmpeg CPU color conversion/RGB path в playback;
- использовать FFmpeg hardware decode/hwaccel API.
