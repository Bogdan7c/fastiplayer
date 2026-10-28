#!/usr/bin/env bash
# Генерация фикстуры MP4 для теста `crates/symphonia-demux/tests/mp4_decode_point_before_seek.rs`.
#
# Запуск: crates/symphonia-demux/tests/fixtures/seek-gop/generate.sh
# Требуется ffmpeg с libx264 и нативным энкодером aac. Тесты ffmpeg не запускают: они читают
# закоммиченный файл. Источники синтетические, ничего не скачивается.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"

# 30 fps, IDR-кадр РОВНО каждые 2.000 с (`keyint=60`, без scenecut), B-frames (`-bf 2`): те же
# свойства, что у живого 1080p-файла, на котором seek приземлялся на 6 с раньше цели.
# Разрешение и битрейт минимальные, чтобы фикстура была крошечной. 20 с = 10 GOP.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=duration=20:size=32x32:rate=30" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=20" \
    -map 0:v -map 1:a -pix_fmt yuv420p \
    -c:v libx264 -preset veryfast -profile:v high -bf 2 -crf 45 \
    -x264-params keyint=60:min-keyint=60:scenecut=0 \
    -c:a aac -b:a 16k -ac 1 -movflags +faststart \
    "$here/gop2s-bframes.mp4"
