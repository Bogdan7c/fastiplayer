#!/usr/bin/env bash
# Генерация фикстур склейки MPEG-TS для crates/mpeg-ts-demux/tests/concatenated_transport_stream.rs
# и сквозного теста media-source-open `local_media/ts_concatenation_tests.rs`.
#
# Запуск: crates/mpeg-ts-demux/tests/fixtures/concatenated/generate.sh
# Требуется ffmpeg с libx264 и энкодерами ac3/aac. Источники синтетические
# (testsrc2, синус), ничего не скачивается. Сами склейки (`cat a b`) и потери
# пакетов тесты собирают в памяти: в репозитории лежат только независимые части.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
duration=0.8
video=(-f lavfi -i "testsrc2=size=64x64:rate=10:duration=${duration}")
tone() { echo "sine=frequency=$1:sample_rate=48000:duration=${duration}"; }
x264=(-c:v libx264 -preset ultrafast -tune zerolatency -g 5 -pix_fmt yuv420p)
common=(-hide_banner -loglevel error -y)
output=(-fflags +bitexact -map_metadata -1)

# Две независимые ffmpeg-записи с одинаковыми параметрами: те же PID-ы, кодеки и
# SPS/PPS, timestamps каждой начинаются с 1.4 с, continuity counters — с нуля.
# Отличается только тон, чтобы склейку было слышно по PCM.
ffmpeg "${common[@]}" "${video[@]}" -f lavfi -i "$(tone 440)" \
    "${x264[@]}" -ac 2 -c:a ac3 -b:a 96k \
    "${output[@]}" -f mpegts "$here/part-a-h264-ac3-440hz.ts"
ffmpeg "${common[@]}" "${video[@]}" -f lavfi -i "$(tone 700)" \
    "${x264[@]}" -ac 2 -c:a ac3 -b:a 96k \
    "${output[@]}" -f mpegts "$here/part-b-h264-ac3-700hz.ts"

# Настоящая смена дорожек: тот же аудио PID, но AAC (stream_type 0x0f) вместо AC-3.
ffmpeg "${common[@]}" "${video[@]}" -f lavfi -i "$(tone 700)" \
    "${x264[@]}" -ac 2 -c:a aac -b:a 96k \
    "${output[@]}" -f mpegts "$here/part-c-h264-aac-700hz.ts"
