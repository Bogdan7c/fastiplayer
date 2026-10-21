#!/usr/bin/env bash
# Генерация фикстуры HEVC-в-MP4 от x265 для сквозного теста
# `crates/symphonia-demux/tests/hevc_x265_mp4.rs`.
#
# Запуск: crates/symphonia-demux/tests/fixtures/hevc-x265/generate.sh
# Требуется ffmpeg с libx265. Тесты ffmpeg не запускают: они читают закоммиченный файл.
# Источник синтетический (testsrc), ничего не скачивается.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"

# x265 по умолчанию пишет в `hvcC` prefix-SEI со строкой своих настроек, поэтому запись
# весит ~2,5 КиБ — больше прежнего лимита 1 КиБ в isomp4 (именно так выглядят файлы
# ffmpeg/HandBrake). `-tag:v hvc1` — sample entry с параметрами только в `hvcC`.
# 6 кадров, ключевой каждые 3: два GOP для проверки ключевых кадров.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=duration=0.25:size=64x64:rate=24" \
    -vf format=yuv420p -c:v libx265 -preset ultrafast -x265-params "keyint=3:min-keyint=3:log-level=error" \
    -tag:v hvc1 -an -movflags +faststart \
    "$here/x265-large-hvcc.mp4"
