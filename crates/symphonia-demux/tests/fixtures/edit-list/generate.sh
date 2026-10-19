#!/usr/bin/env bash
# Генерация фикстуры MP4 с задержанным аудио для сквозного теста
# `crates/symphonia-demux/tests/edit_list_av_sync.rs`.
#
# Запуск: crates/symphonia-demux/tests/fixtures/edit-list/generate.sh
# Требуется ffmpeg с libx264 и нативным энкодером aac. Тесты ffmpeg не запускают: они читают
# закоммиченный файл. Источники синтетические (testsrc, синус), ничего не скачивается.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"

# `-itsoffset 0.25` у аудио-входа заставляет ffmpeg записать ведущую ПУСТУЮ правку (`elst`,
# media_time = -1) в аудио-дорожку; `-bf 2` даёт видео с B-frame, поэтому его `elst` содержит
# обычную правку с media_time > 0 (задержка первого CTS). Именно обе формы одновременно
# пишут реальные файлы ffmpeg, и обе нужны для синхронизации A/V.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=duration=0.5:size=32x32:rate=24" \
    -itsoffset 0.25 -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.5" \
    -map 0:v -map 1:a -vf format=yuv420p \
    -c:v libx264 -preset veryfast -profile:v main -bf 2 -g 12 -b:v 20k \
    -c:a aac -b:a 16k -movflags +faststart \
    "$here/audio-delayed-bframes.mp4"
