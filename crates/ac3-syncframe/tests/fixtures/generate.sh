#!/usr/bin/env bash
# Генерация фикстур для crates/ac3-syncframe/tests/real_frames.rs.
#
# Запуск: crates/ac3-syncframe/tests/fixtures/generate.sh
# Требуется ffmpeg с энкодерами ac3/eac3 (проверено на n8/n9). Звук — синтетический
# синус 440 Гц длительностью 0.1 с (свободная лицензия, ничего не скачивается).
# Байты не побитово стабильны между версиями ffmpeg, поэтому тесты проверяют только
# заголовочные свойства, перечисленные в README.md (частота, каналы, длины кадров).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"

encode() {
    local codec="$1" rate="$2" channels="$3" bitrate="$4" output="$5"
    ffmpeg -hide_banner -loglevel error -y \
        -f lavfi -i "sine=frequency=440:sample_rate=${rate}:duration=0.1" \
        -ac "$channels" -c:a "$codec" -b:a "$bitrate" -f "$codec" "$here/$output"
}

encode ac3 48000 2 192k stereo-48k-192k.ac3
encode ac3 44100 6 448k 5p1-44k1-448k.ac3
encode ac3 32000 1 96k mono-32k-96k.ac3
encode eac3 48000 6 448k 5p1-48k-448k.eac3
encode eac3 44100 2 160k stereo-44k1-160k.eac3
