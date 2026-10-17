#!/usr/bin/env bash
# Генерация фикстур HLS «несколько версий звука» для
# crates/media-source-open/src/native_startup/hls/audio_versions_vertical_tests.rs.
#
# Запуск: crates/media-source-open/tests/fixtures/hls-audio-versions/generate.sh
# Требуется ffmpeg с libx264 и энкодерами aac/eac3. Источники синтетические (testsrc2,
# синус), ничего не скачивается. Байты зависят от версии ffmpeg, поэтому тесты опираются
# только на свойства из README.md.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
common=(-hide_banner -loglevel error -y)
output=(-fflags +bitexact -map_metadata -1 -f mpegts)

# Видео без звука: main variant с отдельными audio renditions (HLS VideoOnly layout), 0.5 с.
ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=64x64:rate=10:duration=0.5" \
    -c:v libx264 -preset ultrafast -tune zerolatency -g 5 -pix_fmt yuv420p \
    "${output[@]}" "$here/video-only-h264.ts"

# Версия 1: AAC-LC 2.0, 48 кГц, 440 Гц.
ffmpeg "${common[@]}" -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.5" \
    -ac 2 -c:a aac -b:a 96k \
    "${output[@]}" "$here/audio-aac-2ch.ts"

# Версия 2: E-AC-3 5.1, 48 кГц, 880 Гц (другая частота — чтобы отличать версии по звуку).
ffmpeg "${common[@]}" -f lavfi -i "sine=frequency=880:sample_rate=48000:duration=0.5" \
    -ac 6 -c:a eac3 -b:a 192k \
    "${output[@]}" "$here/audio-eac3-5p1.ts"
