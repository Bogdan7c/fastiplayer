#!/usr/bin/env bash
# Генерация фикстуры AV1 для сквозного теста программного AV1
# `crates/video-ffmpeg/tests/av1_software_decode.rs`.
#
# Запуск: crates/video-ffmpeg/tests/fixtures/av1/generate.sh
# Требуется ffmpeg с libaom-av1. Тесты ffmpeg не запускают: они читают закоммиченный
# файл. Источник синтетический (testsrc2), ничего не скачивается.
#
# Формат IVF выбран намеренно: каждый его кадр — ровно один AV1 temporal unit
# (низкоуровневый формат OBU), как packet от демуксера. Разбор IVF занимает
# десяток строк в тесте и не тянет в video-ffmpeg зависимость от демуксера.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"

# 160x90, 10 кадров по 5 fps, ключевой кадр каждые 5 кадров: файл в пару КБ,
# но в нём есть и intra-, и inter-кадры. 8-bit 4:2:0 = AV1 Main.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=duration=2:size=160x90:rate=5" \
    -vf format=yuv420p \
    -c:v libaom-av1 -cpu-used 8 -crf 40 -g 5 -still-picture 0 \
    -f ivf "$here/testsrc2-160x90-10frames.ivf"
